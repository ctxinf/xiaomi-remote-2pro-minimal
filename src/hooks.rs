//! Hook system for voice recording events.
//!
//! Modeled on the Codex / Claude Code hook systems: a hook is an external script
//! (shell, Python, Node, ...) that the gateway executes when an event fires.
//!
//! Events:
//! - `start` — a voice stream opened; no audio file exists yet.
//! - `end`   — a voice stream closed **and** produced a valid audio file.
//!
//! The script receives the event payload twice, for convenience:
//! - as JSON on stdin, e.g.
//!   `{"event":"end","audio_file":"/tmp/.../rec.wav","duration_sec":1.83,...}`
//! - as `XIAOMI_*` environment variables (`XIAOMI_EVENT`, `XIAOMI_AUDIO_FILE`, ...).
//!
//! Hooks run detached from the audio path: a failing or slow hook never stalls
//! recording. The `end` hook may move, upload, or remove the file; retention
//! cleanup may eventually remove older recordings.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A recording lifecycle event that hooks can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    /// Voice stream opened.
    Start,
    /// Voice stream closed with a valid audio file on disk.
    End,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::Start => "start",
            HookEvent::End => "end",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "start" => Some(HookEvent::Start),
            "end" => Some(HookEvent::End),
            _ => None,
        }
    }
}

impl std::fmt::Display for HookEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Context handed to a hook script for one event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookContext {
    /// Absolute path of the recorded audio file. Always set for `end`.
    pub audio_file: Option<PathBuf>,
    /// Recording length in seconds. Only meaningful for `end`.
    pub duration_sec: f64,
    /// Total PCM sample count. Only meaningful for `end`.
    pub samples: u64,
    /// PCM sample rate of the recording, in Hz.
    pub sample_rate: u32,
}

impl HookContext {
    /// Renders the payload as a JSON object; also the source of the env vars.
    pub fn to_json(&self, event: HookEvent) -> String {
        let audio = self
            .audio_file
            .as_ref()
            .map(|p| format!("\"{}\"", escape_json(&p.display().to_string())))
            .unwrap_or_else(|| "null".into());
        format!(
            "{{\"event\":\"{}\",\"audio_file\":{},\"duration_sec\":{:.3},\"samples\":{},\"sample_rate\":{}}}",
            event, audio, self.duration_sec, self.samples, self.sample_rate
        )
    }

    fn env_vars(&self, event: HookEvent) -> Vec<(&'static str, String)> {
        let mut vars = vec![
            ("XIAOMI_EVENT", event.to_string()),
            ("XIAOMI_SAMPLE_RATE", self.sample_rate.to_string()),
        ];
        if let Some(path) = &self.audio_file {
            vars.push(("XIAOMI_AUDIO_FILE", path.display().to_string()));
            vars.push(("XIAOMI_DURATION_SEC", format!("{:.3}", self.duration_sec)));
            vars.push(("XIAOMI_SAMPLES", self.samples.to_string()));
        }
        vars
    }
}

/// Escapes the subset of characters that can appear in a filesystem path.
fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Hook scripts registered per event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookConfig {
    hooks: HashMap<HookEvent, Vec<PathBuf>>,
}

impl HookConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.values().all(|v| v.is_empty())
    }

    pub fn add(&mut self, event: HookEvent, script: PathBuf) {
        self.hooks.entry(event).or_default().push(script);
    }

    pub fn scripts_for(&self, event: HookEvent) -> &[PathBuf] {
        self.hooks.get(&event).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Parses a `--hook` CLI value of the form `event=/path/to/script`.
    ///
    /// A bare `/path/to/script` (no `event=` prefix) registers for `end`, the
    /// event that carries the audio file and is what most hooks want.
    pub fn parse_arg(arg: &str) -> anyhow::Result<(HookEvent, PathBuf)> {
        match arg.split_once('=') {
            Some((event, path)) => {
                let parsed = HookEvent::parse(event).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Unknown hook event \"{event}\" (expected \"start\" or \"end\")"
                    )
                })?;
                if path.trim().is_empty() {
                    anyhow::bail!("Hook \"{arg}\" is missing a script path");
                }
                Ok((parsed, PathBuf::from(path)))
            }
            None => {
                if arg.trim().is_empty() {
                    anyhow::bail!("Hook argument is empty");
                }
                Ok((HookEvent::End, PathBuf::from(arg)))
            }
        }
    }

    /// Loads hooks from a JSONC config file.
    ///
    /// ```json
    /// { "hooks": { "start": "./on_start.sh", "end": ["./transcribe.py", "./notify.js"] } }
    /// ```
    ///
    /// Relative script paths resolve against the config file's directory, so a
    /// config can be moved around with its scripts.
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let value = crate::config::read(path)?;

        // Accept both `{"hooks": {...}}` and a bare `{...}` mapping.
        let table = value.get("hooks").unwrap_or(&value);
        let table = table.as_object().ok_or_else(|| {
            anyhow::anyhow!(
                "Hook config {} must be a JSON object mapping events to scripts",
                path.display()
            )
        })?;

        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let mut config = Self::new();
        for (key, entry) in table {
            let event = HookEvent::parse(key).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown hook event \"{key}\" in {} (expected \"start\" or \"end\")",
                    path.display()
                )
            })?;
            let scripts: Vec<&str> = match entry {
                serde_json::Value::String(s) => vec![s.as_str()],
                serde_json::Value::Array(items) => items
                    .iter()
                    .map(|i| {
                        i.as_str().ok_or_else(|| {
                            anyhow::anyhow!(
                                "Hook entry for \"{key}\" in {} must be a string path",
                                path.display()
                            )
                        })
                    })
                    .collect::<anyhow::Result<_>>()?,
                _ => anyhow::bail!(
                    "Hook entry for \"{key}\" in {} must be a string or array of strings",
                    path.display()
                ),
            };
            for script in scripts {
                anyhow::ensure!(
                    !script.trim().is_empty(),
                    "Hook script path cannot be empty"
                );
                let script = PathBuf::from(script);
                let script = if script.is_absolute() {
                    script
                } else {
                    // Strip a leading `./` so the joined path stays readable in logs.
                    base.join(script.strip_prefix("./").unwrap_or(&script))
                };
                config.add(event, script);
            }
        }
        Ok(config)
    }

    /// Merges another config into this one, appending its scripts per event.
    pub fn merge(&mut self, other: HookConfig) {
        for (event, scripts) in other.hooks {
            self.hooks.entry(event).or_default().extend(scripts);
        }
    }

    /// Fires every script registered for `event`, spawning each detached.
    ///
    /// Returns immediately; hook failures are logged, never propagated, so a
    /// broken hook cannot interrupt the next recording.
    pub fn fire(&self, event: HookEvent, ctx: &HookContext) {
        let scripts = self.scripts_for(event);
        if scripts.is_empty() {
            return;
        }
        let payload = ctx.to_json(event);
        let env = ctx.env_vars(event);
        for script in scripts {
            match spawn_hook(script, &payload, &env) {
                Ok(()) => log::info!("[HOOK] {event} -> {}", script.display()),
                Err(e) => log::error!("[HOOK] {event} -> {} failed: {e}", script.display()),
            }
        }
    }
}

/// Makes a hook path absolute against the current working directory.
fn resolve(script: &Path) -> PathBuf {
    if script.is_absolute() {
        return script.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(script))
        .unwrap_or_else(|_| script.to_path_buf())
}

/// Explains a spawn failure in terms the user can act on.
fn spawn_error(script: &Path, e: std::io::Error) -> anyhow::Error {
    let hint = match e.kind() {
        std::io::ErrorKind::NotFound if !script.exists() => " (file does not exist)",
        // The file is there, so ENOENT is the kernel failing to find the
        // interpreter named by the script's shebang line.
        std::io::ErrorKind::NotFound => {
            " (file exists; check its #! shebang line points to a real interpreter)"
        }
        std::io::ErrorKind::PermissionDenied => " (not executable; run: chmod +x <script>)",
        _ => "",
    };
    anyhow::anyhow!("could not execute {}: {e}{hint}", script.display())
}

/// Forwards each line a hook writes into the gateway log, prefixed with its name.
fn relay(stream: impl std::io::Read, label: &str, is_stderr: bool) {
    use std::io::BufRead as _;
    for line in std::io::BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if is_stderr {
            log::warn!("[HOOK:{label}] {line}");
        } else {
            log::info!("[HOOK:{label}] {line}");
        }
    }
}

/// Spawns one hook script, writing the JSON payload to its stdin.
fn spawn_hook(script: &Path, payload: &str, env: &[(&'static str, String)]) -> anyhow::Result<()> {
    // Resolve before spawning: the program path would otherwise be interpreted
    // relative to `current_dir` below, so a relative script would be looked up
    // inside its own directory and never found.
    let script = resolve(script);

    let mut command = Command::new(&script);
    command
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .stdin(Stdio::piped())
        // Captured rather than inherited so hook output is tagged in the log and
        // cannot interleave mid-line with the gateway's own output.
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Run the hook from its own directory so relative paths inside the script
    // resolve somewhere predictable.
    if let Some(parent) = script.parent() {
        if !parent.as_os_str().is_empty() {
            command.current_dir(parent);
        }
    }

    let started = std::time::Instant::now();
    log::debug!(
        "[HOOK] Spawning {} (payload {} bytes)",
        script.display(),
        payload.len()
    );
    let mut child = command.spawn().map_err(|e| spawn_error(&script, e))?;

    let pid = child.id();
    log::debug!("[HOOK] Spawned {} pid={pid}", script.display());
    if let Some(mut stdin) = child.stdin.take() {
        let payload = payload.to_string();
        // Write on a detached thread: a hook that never reads stdin would
        // otherwise block the audio thread once the pipe buffer fills.
        std::thread::spawn(move || {
            if let Err(e) = stdin.write_all(payload.as_bytes()) {
                log::debug!("[HOOK] pid={pid} stdin delivery failed: {e}");
            }
        });
    }

    // Relay the hook's own output into the log, then reap it so finished hooks
    // do not linger as zombies.
    let label = script
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| script.display().to_string());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    std::thread::spawn(move || {
        // stderr on its own thread so a hook writing heavily to one stream
        // cannot block the other.
        let err_label = label.clone();
        let err_thread =
            stderr.map(|stderr| std::thread::spawn(move || relay(stderr, &err_label, true)));

        if let Some(stdout) = stdout {
            relay(stdout, &label, false);
        }
        if let Some(t) = err_thread {
            let _ = t.join();
        }

        match child.wait() {
            Ok(status) if status.success() => log::debug!(
                "[HOOK] {label} pid={pid} finished in {:?}",
                started.elapsed()
            ),
            Ok(status) => log::warn!(
                "[HOOK] {label} pid={pid} exited with {status} after {:?}",
                started.elapsed()
            ),
            Err(e) => log::warn!("[HOOK] {label} could not be waited on: {e}"),
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_arg_defaults_to_end() {
        let (event, path) = HookConfig::parse_arg("./on_audio.sh").unwrap();
        assert_eq!(event, HookEvent::End);
        assert_eq!(path, PathBuf::from("./on_audio.sh"));
    }

    #[test]
    fn parse_arg_reads_event_prefix() {
        let (event, path) = HookConfig::parse_arg("start=/opt/hooks/begin.py").unwrap();
        assert_eq!(event, HookEvent::Start);
        assert_eq!(path, PathBuf::from("/opt/hooks/begin.py"));
        assert!(HookConfig::parse_arg("bogus=/x.sh").is_err());
    }

    #[test]
    fn json_payload_contains_audio_file() {
        let ctx = HookContext {
            audio_file: Some(PathBuf::from("/tmp/rec.wav")),
            duration_sec: 1.5,
            samples: 24_000,
            sample_rate: 16_000,
        };
        let json = ctx.to_json(HookEvent::End);
        assert!(json.contains("\"event\":\"end\""));
        assert!(json.contains("\"audio_file\":\"/tmp/rec.wav\""));
        assert!(json.contains("\"samples\":24000"));
    }

    #[test]
    fn start_payload_has_null_audio_file() {
        let json = HookContext {
            sample_rate: 16_000,
            ..Default::default()
        }
        .to_json(HookEvent::Start);
        assert!(json.contains("\"audio_file\":null"));
    }
}
