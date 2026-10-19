//! Xiaomi Remote Pro 2 Gateway CLI Application.
//!
//! Records voice from the remote's microphone button to temporary WAV files and
//! triggers hook scripts on the `start` / `end` recording events.

use anyhow::Context as _;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use futures_lite::StreamExt;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use xiaomi_remote_pro2_gateway::atvv;
use xiaomi_remote_pro2_gateway::ble_bridge::{self, BridgeConfig};
use xiaomi_remote_pro2_gateway::bluest::Adapter;
use xiaomi_remote_pro2_gateway::hooks::{HookConfig, HookEvent};
use xiaomi_remote_pro2_gateway::recorder::{default_tmp_dir, WavRecorder};
use xiaomi_remote_pro2_gateway::retention::RetentionPolicy;
use xiaomi_remote_pro2_gateway::voice_key;

/// env_logger serializes writes, keeping terminal and file records together.
struct LogOutput(std::fs::File);

impl std::io::Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write_all(bytes)?;
        // A closed terminal must not prevent logs from reaching the file.
        let _ = std::io::stderr().write_all(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()?;
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "xiaomi-remote-gateway",
    version,
    about = "Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Voice Gateway: records mic audio and triggers hooks"
)]
struct Cli {
    /// JSONC gateway configuration (CLI options take precedence)
    #[arg(long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Enable all diagnostic logging (including protocol trace)
    #[arg(long = "debug", visible_alias = "verbose", short = 'v', global = true)]
    verbose: bool,

    /// Append logs to a file as well as stderr
    #[arg(long, value_name = "FILE", global = true)]
    log_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,

    /// Directory for recorded audio WAV files [default: $TMPDIR/xiaomi-remote-gateway]
    #[arg(long, global = true)]
    record_dir: Option<PathBuf>,

    /// Hook script to run on an event: `end=./script.sh`, `start=./script.py`,
    /// or a bare path (defaults to the `end` event). Repeatable.
    #[arg(long = "hook", value_name = "[EVENT=]SCRIPT", global = true)]
    hooks: Vec<String>,

    /// JSONC file declaring hooks, e.g. {"hooks":{"end":["./transcribe.py"]}}
    #[arg(long, value_name = "FILE", global = true)]
    hook_config: Option<PathBuf>,

    /// Audio digital gain in dB (-24.0 to +24.0)
    #[arg(
        long,
        default_value_t = 0.0,
        allow_negative_numbers = true,
        global = true
    )]
    gain: f64,

    /// Reconnect interval in seconds
    #[arg(long, default_value_t = 3, global = true)]
    reconnect: u64,

    /// Target device name filter (optional)
    #[arg(long, global = true)]
    device_name: Option<String>,

    /// Delete recordings older than this many hours (0 = keep forever).
    /// Recordings are meant to be consumed by the `end` hook; this reaps
    /// whatever the hook left behind.
    #[arg(long, default_value_t = 24, global = true)]
    keep_hours: u64,

    /// Keep at most this many recordings, newest first (0 = unlimited)
    #[arg(long, default_value_t = 100, global = true)]
    keep_max: usize,

    /// Mic-button suppression: `auto` (default) grabs the remote only when no
    /// key remapper such as keyd already owns it, `always` forces the grab,
    /// `never` leaves the input layer alone.
    #[arg(
        long = "voice-key",
        value_name = "auto|always|never",
        default_value = "auto",
        global = true
    )]
    voice_key: String,

    /// Deprecated alias for `--voice-key never`.
    #[arg(long, global = true, hide = true)]
    no_suppress_voice_key: bool,
}

impl Cli {
    /// Resolves the suppression mode, honouring the deprecated flag.
    fn voice_key_mode(&self) -> anyhow::Result<voice_key::Mode> {
        let mode = voice_key::Mode::parse(&self.voice_key).with_context(|| {
            format!(
                "invalid --voice-key value {:?} (expected auto, always, or never)",
                self.voice_key
            )
        })?;
        if self.no_suppress_voice_key {
            log::warn!("[GATEWAY] --no-suppress-voice-key is deprecated; use --voice-key never.");
            return Ok(voice_key::Mode::Never);
        }
        Ok(mode)
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Record voice from the remote and trigger hooks (default)
    Run,
    /// Scan for nearby BLE devices and identify Xiaomi remotes
    Scan {
        /// Scan duration in seconds
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Some(path) = cli.config.clone() {
        let value = xiaomi_remote_pro2_gateway::config::read(&path)?;
        let object = value
            .as_object()
            .context("Gateway configuration must be an object")?;
        for (key, value) in object {
            if key == "hooks" {
                continue;
            }
            let id = match key.as_str() {
                "record_dir" | "gain" | "reconnect" | "device_name" | "keep_hours" | "keep_max"
                | "voice_key" => key.as_str(),
                _ => anyhow::bail!("Unknown gateway configuration option: {key}"),
            };
            if matches.value_source(id) == Some(clap::parser::ValueSource::CommandLine) {
                continue;
            }
            match id {
                "record_dir" => {
                    let dir = PathBuf::from(value.as_str().context("record_dir must be a string")?);
                    cli.record_dir = Some(if dir.is_absolute() {
                        dir
                    } else {
                        path.parent().unwrap_or(std::path::Path::new(".")).join(dir)
                    });
                }
                "gain" => cli.gain = value.as_f64().context("gain must be a number")?,
                "reconnect" => {
                    cli.reconnect = value
                        .as_u64()
                        .context("reconnect must be a nonnegative integer")?
                }
                "keep_hours" => {
                    cli.keep_hours = value
                        .as_u64()
                        .context("keep_hours must be a nonnegative integer")?
                }
                "keep_max" => {
                    cli.keep_max = usize::try_from(
                        value
                            .as_u64()
                            .context("keep_max must be a nonnegative integer")?,
                    )?
                }
                "device_name" => {
                    cli.device_name = Some(
                        value
                            .as_str()
                            .context("device_name must be a string")?
                            .into(),
                    )
                }
                "voice_key" => {
                    cli.voice_key = value.as_str().context("voice_key must be a string")?.into()
                }
                _ => unreachable!(),
            }
        }
    }
    anyhow::ensure!(
        cli.gain.is_finite() && (-24.0..=24.0).contains(&cli.gain),
        "gain must be between -24 and 24 dB"
    );

    let mut logger = env_logger::Builder::new();
    if let Some(path) = &cli.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Cannot open log file {}", path.display()))?;
        logger.target(env_logger::Target::Pipe(Box::new(LogOutput(file))));
        logger.write_style(env_logger::WriteStyle::Never);
    }
    logger
        .filter_level(log::LevelFilter::Warn)
        .filter_module(
            "xiaomi_remote_pro2_gateway",
            if cli.verbose {
                log::LevelFilter::Trace
            } else {
                log::LevelFilter::Info
            },
        )
        .format_timestamp_millis()
        .init();

    log::info!("===========================================================");
    log::info!("  Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Voice Gateway");
    log::info!("===========================================================");

    if let Some(path) = &cli.log_file {
        log::info!("[GATEWAY] Appending logs to {}", path.display());
    }
    let result = match cli.command.take().unwrap_or(Command::Run) {
        Command::Scan { seconds } => cmd_scan(seconds).await,
        Command::Run => cmd_run(cli).await,
    };
    if let Err(error) = &result {
        log::error!("[GATEWAY] Exiting: {error:#}");
    }
    result
}

/// Builds the hook set from the config file first, then the `--hook` flags, so
/// command-line hooks run after the ones declared in the config.
fn build_hooks(cli: &Cli) -> anyhow::Result<HookConfig> {
    let mut hooks = match &cli.hook_config {
        Some(path) => HookConfig::from_file(path)?,
        None => HookConfig::new(),
    };
    if let Some(path) = &cli.config {
        let value = xiaomi_remote_pro2_gateway::config::read(path)?;
        if value.get("hooks").is_some() {
            hooks.merge(HookConfig::from_file(path)?);
        }
    }
    for arg in &cli.hooks {
        let (event, script) = HookConfig::parse_arg(arg)?;
        hooks.add(event, script);
    }
    Ok(hooks)
}

async fn setup_shutdown_handler() -> CancellationToken {
    let shutdown = CancellationToken::new();
    let s_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("[GATEWAY] Ctrl-C received, shutting down gracefully...");
        s_clone.cancel();
    });
    shutdown
}

async fn cmd_scan(seconds: u64) -> anyhow::Result<()> {
    let adapter = Adapter::default()
        .await
        .context("No Bluetooth adapter found on system")?;
    adapter
        .wait_available()
        .await
        .context("Bluetooth adapter is not available or turned off")?;

    println!("\nChecking system-connected devices with ATVV Voice Service...");
    if let Ok(devices) = adapter
        .connected_devices_with_services(&[atvv::SERVICE])
        .await
    {
        if !devices.is_empty() {
            for d in &devices {
                let name = d.name().unwrap_or_else(|_| "(unnamed)".into());
                println!("  ★ [Connected] {} [{}]", name, d.id());
            }
        } else {
            println!("  (No ATVV remotes currently connected in system Bluetooth)");
        }
    }

    println!("\nScanning BLE advertisements for {seconds} seconds...");
    println!("Tip: If remote is not paired, long-press [Home] + [Menu] on the remote.\n");

    let mut scan = adapter.scan(&[]).await?;
    let deadline = tokio::time::sleep(Duration::from_secs(seconds));
    tokio::pin!(deadline);
    let mut seen = HashSet::new();

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            _ = tokio::signal::ctrl_c() => break,
            item = scan.next() => {
                let Some(adv) = item else { break };
                if !seen.insert(adv.device.id()) {
                    continue;
                }
                let is_cand = ble_bridge::advertisement_is_candidate(&adv);
                let name = adv.adv_data.local_name
                    .clone()
                    .or_else(|| adv.device.name().ok())
                    .unwrap_or_else(|| "(unnamed)".into());

                if is_cand {
                    println!("  ★ Target Remote: {:<30} [{}] RSSI: {:?}", name, adv.device.id(), adv.rssi);
                } else if log::log_enabled!(log::Level::Debug) {
                    println!("    Other Device:  {:<30} [{}] RSSI: {:?}", name, adv.device.id(), adv.rssi);
                }
            }
        }
    }

    println!("\nScan completed.");
    Ok(())
}

async fn cmd_run(cli: Cli) -> anyhow::Result<()> {
    let hooks = build_hooks(&cli)?;
    let voice_key_mode = cli.voice_key_mode()?;
    let record_dir = cli.record_dir.clone().unwrap_or_else(default_tmp_dir);

    let retention = RetentionPolicy::from_args(cli.keep_hours, cli.keep_max);

    log::info!("[GATEWAY] Record directory: {}", record_dir.display());
    log::info!("[GATEWAY] Retention: {}", retention.describe());
    log::info!("[GATEWAY] Digital Gain: {:.1} dB", cli.gain);
    log::info!("[GATEWAY] Reconnect delay: {}s", cli.reconnect);
    log::info!("[GATEWAY] Mic-key suppression: {voice_key_mode}");
    for event in [HookEvent::Start, HookEvent::End] {
        for script in hooks.scripts_for(event) {
            log::info!("[GATEWAY] Hook [{event}]: {}", script.display());
        }
    }
    if hooks.is_empty() {
        log::info!("[GATEWAY] No hooks configured (use --hook or --hook-config).");
    }

    let shutdown = setup_shutdown_handler().await;

    // Swallow the mic button's HID report so it cannot double as a desktop
    // keypress while the BLE side is recording. In `auto` this yields to keyd
    // and friends rather than fighting them for the exclusive grab.
    let suppressor = tokio::spawn(voice_key::run(shutdown.clone(), voice_key_mode));

    let mut recorder = WavRecorder::new(record_dir, hooks, retention);
    // Clear out anything previous runs left behind before recording more.
    recorder.sweep_startup();
    let config = BridgeConfig {
        gain_db: cli.gain,
        reconnect_delay: Duration::from_secs(cli.reconnect.max(1)),
        scan_timeout: Duration::from_secs(10),
        target_name: cli.device_name,
    };

    let result = ble_bridge::run_ble_bridge(&mut recorder, &config, &shutdown).await;

    // Ensure the grab is released before exiting.
    shutdown.cancel();
    let _ = suppressor.await;

    result
}
