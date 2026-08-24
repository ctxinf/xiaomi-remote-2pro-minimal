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

use tokio_util::sync::CancellationToken;

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

    fn is_voice_key(key: KeyCode) -> bool {
        VOICE_KEYS.contains(&key)
    }

    fn is_target_device(dev: &Device) -> bool {
        let id = dev.input_id();
        if id.vendor() == TARGET_VENDOR && id.product() == TARGET_PRODUCT {
            return true;
        }
        dev.name()
            .is_some_and(|name| KNOWN_NAMES.iter().any(|known| name.contains(known)))
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

    pub async fn run(shutdown: CancellationToken) {
        // The remote comes and goes, so attach() is retried; log each distinct
        // failure once instead of repeating it every couple of seconds.
        let mut last_error: Option<String> = None;
        while !shutdown.is_cancelled() {
            match attach(&shutdown).await {
                Ok(()) => last_error = None,
                Err(e) => {
                    let msg = format!("{e:#}");
                    if last_error.as_deref() != Some(msg.as_str()) {
                        log::warn!("[VOICE KEY] {msg}");
                        log::warn!("[VOICE KEY] Mic button will keep reaching the desktop until this is fixed (or pass --no-suppress-voice-key to silence this).");
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
    async fn attach(shutdown: &CancellationToken) -> anyhow::Result<()> {
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
                "\n==> Another process already holds it exclusively \
                 — usually a second copy of this gateway. Stop it and retry."
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

    pub async fn run(shutdown: CancellationToken) {
        log::info!("[VOICE KEY] Voice-button suppression is only supported on Linux.");
        shutdown.cancelled().await;
    }
}

/// Runs voice-button suppression until `shutdown` is cancelled.
pub async fn run(shutdown: CancellationToken) {
    #[cfg(target_os = "linux")]
    linux_impl::run(shutdown).await;

    #[cfg(not(target_os = "linux"))]
    other_impl::run(shutdown).await;
}
