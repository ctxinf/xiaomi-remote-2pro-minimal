//! Voice-button suppression.
//!
//! The remote reports the microphone button over plain HID *in addition to* the
//! BLE ATVV voice event, so pressing it also reaches the desktop: on this remote
//! it arrives as `KEY_F5` (which starts the VS Code debugger) and `KEY_F20`.
//!
//! This module takes exclusive control of the remote's input device
//! (`EVIOCGRAB`) and forwards every event back out through a uinput clone —
//! except the voice keys, which are dropped. Navigation, volume, and OK keep
//! working as normal; only the mic button stops leaking through.
//!
//! # Playing nice with keyd (and anything else that grabs the remote)
//!
//! `EVIOCGRAB` is exclusive: only one process can hold a given `/dev/input/event*`
//! at a time. A key remapper such as [keyd] works exactly the same way — it grabs
//! the physical device and republishes events through its own uinput device. So if
//! keyd already handles the remote (including already dropping the mic key), this
//! module must **not** try to grab it too:
//!
//! - it would fail with `EBUSY` and log a recurring warning about nothing, and
//! - worse, whoever grabs first on the next reconnect wins, so we could silently
//!   steal the device from keyd and void the user's entire remapping config.
//!
//! Hence [`Mode::Auto`] (the default): probe for an external suppressor first and
//! stand down when one is found. `Mode::Always` forces the grab for setups with no
//! remapper, `Mode::Never` disables this module entirely.
//!
//! [keyd]: https://github.com/rvaiya/keyd

use tokio_util::sync::CancellationToken;

/// How hard this module should try to swallow the mic button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Grab the remote only when nothing else (keyd, another gateway) owns it.
    #[default]
    Auto,
    /// Always grab, even if an external remapper is detected.
    Always,
    /// Never touch the input layer; the mic key reaches the desktop unless
    /// something outside the gateway filters it.
    Never,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Mode::Auto),
            "always" | "grab" | "on" => Some(Mode::Always),
            "never" | "off" | "none" => Some(Mode::Never),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Auto => "auto",
            Mode::Always => "always",
            Mode::Never => "never",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;
    use evdev::uinput::VirtualDevice;
    use evdev::{enumerate, AttributeSet, Device, EventType, InputEvent, KeyCode};
    use std::path::PathBuf;
    use std::time::Duration;

    const TARGET_VENDOR: u16 = 0x2717;
    const TARGET_PRODUCT: u16 = 0x32b8;

    const KNOWN_NAMES: &[&str] = &[
        "小米蓝牙语音遥控器",
        "MI RC",
        "Xiaomi Bluetooth Remote",
        "RC003",
        "ARN9",
    ];

    /// Keycodes the microphone button emits. These are swallowed; everything
    /// else is passed through untouched.
    const VOICE_KEYS: &[KeyCode] = &[
        KeyCode::KEY_F5,          // 63  - what this remote sends, and VS Code's debug key
        KeyCode::KEY_F20,         // 190 - secondary code sent alongside F5
        KeyCode::KEY_SEARCH,      // 217 - used by other firmware revisions
        KeyCode::KEY_VOICECOMMAND,// 582 - Android TV standard voice key
        KeyCode::KEY_MICMUTE,     // 248
    ];

    /// Virtual devices published by other remappers (and by this module itself).
    /// Grabbing one of these would put us *downstream* of the tool that already
    /// owns the remote — a second filter on an already-filtered stream — so they
    /// are never treated as the remote, however closely the name matches.
    const VIRTUAL_DEVICE_MARKERS: &[&str] = &[
        "keyd virtual",
        "voice-filtered", // our own forwarder, see build_forwarder()
        "kmonad",
        "evsieve",
        "interception",
    ];

    /// Files keyd creates while running. Presence means "a remapper owns the
    /// keyboards on this machine", which is the case this probe cares about.
    const KEYD_RUNTIME_PATHS: &[&str] = &["/run/keyd.socket", "/var/run/keyd.socket"];

    fn is_voice_key(key: KeyCode) -> bool {
        VOICE_KEYS.contains(&key)
    }

    fn is_virtual_device(dev: &Device) -> bool {
        dev.name().is_some_and(|name| {
            let lower = name.to_ascii_lowercase();
            VIRTUAL_DEVICE_MARKERS.iter().any(|m| lower.contains(m))
        })
    }

    fn is_target_device(dev: &Device) -> bool {
        if is_virtual_device(dev) {
            return false;
        }
        let id = dev.input_id();
        if id.vendor() == TARGET_VENDOR && id.product() == TARGET_PRODUCT {
            return true;
        }
        dev.name()
            .is_some_and(|name| KNOWN_NAMES.iter().any(|known| name.contains(known)))
    }

    /// Detects a key remapper that has (or will) grab the remote for itself.
    ///
    /// Two independent signals, because either can show up alone: keyd's runtime
    /// socket exists as soon as the daemon starts, while its virtual device only
    /// appears once it has bound something. Returns a human-readable description
    /// of what was found, for the log line.
    fn external_suppressor() -> Option<String> {
        for path in KEYD_RUNTIME_PATHS {
            if std::path::Path::new(path).exists() {
                return Some(format!("keyd is running ({path})"));
            }
        }
        enumerate()
            .find(|(_, dev)| is_virtual_device(dev))
            .map(|(_, dev)| {
                format!(
                    "another input remapper is active (\"{}\")",
                    dev.name().unwrap_or("virtual device")
                )
            })
    }

    fn find_remote_device() -> Option<(PathBuf, String)> {
        enumerate().find(|(_, dev)| is_target_device(dev)).map(|(path, dev)| {
            let name = dev.name().unwrap_or("Xiaomi Remote").to_string();
            (path, name)
        })
    }

    /// Builds the uinput device that replays the non-voice events.
    fn build_forwarder(source: &Device) -> anyhow::Result<VirtualDevice> {
        // Advertise the same keys as the remote, minus the voice keys, so the
        // desktop sees an equivalent keyboard.
        let mut keys = AttributeSet::<KeyCode>::new();
        if let Some(supported) = source.supported_keys() {
            for key in supported.iter().filter(|k| !is_voice_key(*k)) {
                keys.insert(key);
            }
        }

        VirtualDevice::builder()
            .and_then(|b| b.name("Xiaomi Remote (voice-filtered)").with_keys(&keys))
            .and_then(|b| b.build())
            .map_err(|e| {
                anyhow::anyhow!(
                    "could not open /dev/uinput: {e}\n\
                     ==> TIP: grant access without root via:\n\
                     ==>   echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee /etc/udev/rules.d/99-uinput.rules\n\
                     ==>   sudo udevadm control --reload-rules && sudo modprobe uinput"
                )
            })
    }

    pub async fn run(shutdown: CancellationToken, mode: Mode) {
        if mode == Mode::Never {
            log::info!(
                "[VOICE KEY] Suppression disabled (--voice-key never); the mic button is left to the system."
            );
            shutdown.cancelled().await;
            return;
        }

        // Stand down before touching anything when a remapper already owns the
        // keyboards: grabbing would either fail or, worse, win a race against it.
        if mode == Mode::Auto {
            if let Some(found) = external_suppressor() {
                log::info!("[VOICE KEY] {found}; leaving the remote's keys to it.");
                log::info!(
                    "[VOICE KEY] Make sure it drops the mic key (keyd: map it to `noop`). \
                     Force the gateway's own grab instead with --voice-key always."
                );
                shutdown.cancelled().await;
                return;
            }
        }

        // The remote comes and goes, so attach() is retried; log each distinct
        // failure once instead of repeating it every couple of seconds.
        let mut last_error: Option<String> = None;
        while !shutdown.is_cancelled() {
            match attach(&shutdown, mode).await {
                Ok(()) => last_error = None,
                Err(e) => {
                    let msg = format!("{e:#}");
                    if last_error.as_deref() != Some(msg.as_str()) {
                        log::warn!("[VOICE KEY] {msg}");
                        log::warn!("[VOICE KEY] Mic button will keep reaching the desktop until this is fixed (or pass --voice-key never to silence this).");
                        last_error = Some(msg);
                    }
                }
            }
            if shutdown.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
        log::info!("[VOICE KEY] Voice-button suppression stopped.");
    }

    /// Grabs the remote once and pumps events until it disconnects.
    async fn attach(shutdown: &CancellationToken, mode: Mode) -> anyhow::Result<()> {
        // Re-probed on every attach, not just at startup: keyd may well be
        // started (or restarted) after the gateway, and the only safe moment to
        // notice that is before we take the grab.
        if mode == Mode::Auto {
            if let Some(found) = external_suppressor() {
                log::debug!("[VOICE KEY] {found}; skipping grab this round.");
                return Ok(());
            }
        }

        let Some((path, name)) = find_remote_device() else {
            log::debug!("[VOICE KEY] Remote input device not present yet, waiting...");
            return Ok(());
        };

        let mut device = Device::open(&path).map_err(|e| {
            anyhow::anyhow!(
                "cannot open {} ({e}); add your user to the 'input' group",
                path.display()
            )
        })?;

        let mut forwarder = build_forwarder(&device)?;

        device.grab().map_err(|e| {
            let hint = if e.kind() == std::io::ErrorKind::ResourceBusy {
                // EVIOCGRAB is exclusive, so this means keyd (or another
                // remapper, or a second gateway) got there first. Nothing to
                // fix if that tool already drops the mic key.
                "\n==> Another process already holds it exclusively — keyd, another \
                 input remapper, or a second copy of this gateway.\
                 \n==> If that tool already suppresses the mic key, pass \
                 --voice-key never (or leave the default `auto`) and ignore this."
            } else {
                ""
            };
            anyhow::anyhow!(
                "cannot grab {} exclusively (EVIOCGRAB): {e}{hint}",
                path.display()
            )
        })?;

        log::info!(
            "[VOICE KEY] Suppressing voice button on \"{}\" ({}); other keys pass through.",
            name,
            path.display()
        );

        let mut stream = device
            .into_event_stream()
            .map_err(|e| anyhow::anyhow!("cannot stream events from {}: {e}", path.display()))?;

        loop {
            let event = tokio::select! {
                _ = shutdown.cancelled() => break,
                res = stream.next_event() => match res {
                    Ok(ev) => ev,
                    Err(e) => {
                        log::debug!("[VOICE KEY] Input stream ended: {e}");
                        break;
                    }
                }
            };

            if event.event_type() == EventType::KEY && is_voice_key(KeyCode::new(event.code())) {
                if event.value() == 1 {
                    log::info!(
                        "[VOICE KEY] Mic button (code {}) suppressed; not forwarded to desktop.",
                        event.code()
                    );
                }
                continue;
            }

            let out = InputEvent::new(event.event_type().0, event.code(), event.value());
            if let Err(e) = forwarder.emit(&[out]) {
                log::warn!("[VOICE KEY] Failed to forward event: {e}");
            }
        }

        // Releasing the grab restores normal behaviour for the next attach.
        let _ = stream.device_mut().ungrab();
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod other_impl {
    use super::*;

    pub async fn run(shutdown: CancellationToken, _mode: Mode) {
        log::info!("[VOICE KEY] Voice-button suppression is only supported on Linux.");
        shutdown.cancelled().await;
    }
}

/// Runs voice-button suppression until `shutdown` is cancelled.
pub async fn run(shutdown: CancellationToken, mode: Mode) {
    #[cfg(target_os = "linux")]
    linux_impl::run(shutdown, mode).await;

    #[cfg(not(target_os = "linux"))]
    other_impl::run(shutdown, mode).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mode_spellings() {
        assert_eq!(Mode::parse("auto"), Some(Mode::Auto));
        assert_eq!(Mode::parse(" ALWAYS "), Some(Mode::Always));
        assert_eq!(Mode::parse("grab"), Some(Mode::Always));
        assert_eq!(Mode::parse("off"), Some(Mode::Never));
        assert_eq!(Mode::parse("never"), Some(Mode::Never));
        assert_eq!(Mode::parse("maybe"), None);
    }

    #[test]
    fn auto_is_the_default() {
        assert_eq!(Mode::default(), Mode::Auto);
        assert_eq!(Mode::Auto.to_string(), "auto");
    }
}
