//! Xiaomi Remote Pro 2 Gateway CLI Application.

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use futures_lite::StreamExt;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use xiaomi_remote_pro2_gateway::atvv;
use xiaomi_remote_pro2_gateway::ble_bridge::{self, BridgeConfig};
use xiaomi_remote_pro2_gateway::bluest::Adapter;
use xiaomi_remote_pro2_gateway::key_monitor;
use xiaomi_remote_pro2_gateway::recorder::AutoWavRecorder;

#[derive(Parser, Debug)]
#[command(
    name = "xiaomi-remote-gateway",
    version,
    about = "Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Gateway: Key Logging, Voice Recording & Status Display"
)]
struct Cli {
    /// Enable verbose debug logging
    #[arg(long, short = 'v', global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    /// Directory to store recorded audio WAV files
    #[arg(long, default_value = "./records", global = true)]
    record_dir: PathBuf,

    /// Fixed single WAV file path for recording (optional)
    #[arg(long, global = true)]
    wav: Option<PathBuf>,

    /// Audio digital gain in dB (-24.0 to +24.0)
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true, global = true)]
    gain: f64,

    /// Reconnect interval in seconds
    #[arg(long, default_value_t = 3, global = true)]
    reconnect: u64,

    /// Target device name filter (optional)
    #[arg(long, global = true)]
    device_name: Option<String>,

    /// Suppress remote buttons from triggering system actions (exclusive grab: blocks VSCode F5 debug, desktop volume change, etc.)
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set, global = true)]
    grab: bool,

    /// Disable key grabbing (allow remote buttons to pass through to system desktop and apps)
    #[arg(long, global = true)]
    no_grab: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the full gateway: BLE Voice Audio Recorder + Button Event Monitor (Default)
    Run,
    /// Scan for nearby BLE devices and identify Xiaomi remotes
    Scan {
        /// Scan duration in seconds
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
    /// Run only the remote button event monitor
    Keys,
    /// Run only the BLE audio voice recorder
    Record,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    env_logger::Builder::new()
        .filter_level(if cli.verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
        .format_timestamp_millis()
        .init();

    log::info!("===========================================================");
    log::info!("  Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Gateway");
    log::info!("===========================================================");

    let grab = if cli.no_grab { false } else { cli.grab };
    let cmd = cli.command.unwrap_or(Command::Run);

    match cmd {
        Command::Scan { seconds } => cmd_scan(seconds).await,
        Command::Keys => cmd_keys(grab).await,
        Command::Record => {
            cmd_record(cli.record_dir, cli.wav, cli.gain, cli.reconnect, cli.device_name).await
        }
        Command::Run => {
            cmd_run_full(
                cli.record_dir,
                cli.wav,
                cli.gain,
                cli.reconnect,
                cli.device_name,
                grab,
            )
            .await
        }
    }
}

async fn setup_shutdown_handler() -> CancellationToken {
    let shutdown = CancellationToken::new();
    let s_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("\n[GATEWAY] Ctrl-C received, shutting down gracefully...");
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
    if let Ok(devices) = adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
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

async fn cmd_keys(grab: bool) -> anyhow::Result<()> {
    let shutdown = setup_shutdown_handler().await;
    key_monitor::run_key_monitor(shutdown.clone(), grab).await;
    Ok(())
}

async fn cmd_record(
    record_dir: PathBuf,
    wav: Option<PathBuf>,
    gain: f64,
    reconnect: u64,
    device_name: Option<String>,
) -> anyhow::Result<()> {
    let shutdown = setup_shutdown_handler().await;
    let mut recorder = AutoWavRecorder::new(record_dir, wav);

    let config = BridgeConfig {
        gain_db: gain,
        reconnect_delay: Duration::from_secs(reconnect.max(1)),
        scan_timeout: Duration::from_secs(10),
        target_name: device_name,
    };

    ble_bridge::run_ble_bridge(&mut recorder, &config, &shutdown).await
}

async fn cmd_run_full(
    record_dir: PathBuf,
    wav: Option<PathBuf>,
    gain: f64,
    reconnect: u64,
    device_name: Option<String>,
    grab: bool,
) -> anyhow::Result<()> {
    let shutdown = setup_shutdown_handler().await;

    log::info!("[GATEWAY] Record directory: {}", record_dir.display());
    if let Some(ref w) = wav {
        log::info!("[GATEWAY] Fixed WAV output: {}", w.display());
    }
    log::info!("[GATEWAY] Digital Gain: {:.1} dB", gain);
    log::info!("[GATEWAY] Reconnect delay: {}s", reconnect);
    log::info!(
        "[GATEWAY] System key suppression (grab): {}",
        if grab { "ENABLED (已屏蔽系统按键)" } else { "DISABLED (允许系统直通)" }
    );

    // 1. Spawn Key Monitor Task
    let key_shutdown = shutdown.clone();
    let key_handle = tokio::spawn(async move {
        key_monitor::run_key_monitor(key_shutdown, grab).await;
    });

    // 2. Run BLE Bridge Task
    let mut recorder = AutoWavRecorder::new(record_dir, wav);
    let config = BridgeConfig {
        gain_db: gain,
        reconnect_delay: Duration::from_secs(reconnect.max(1)),
        scan_timeout: Duration::from_secs(10),
        target_name: device_name,
    };

    let ble_res = ble_bridge::run_ble_bridge(&mut recorder, &config, &shutdown).await;

    // Await key monitor shutdown
    let _ = key_handle.await;

    ble_res
}
