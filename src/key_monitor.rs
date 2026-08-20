//! Key Event Monitor for Remote Buttons.
//!
//! Listens for remote button events via Linux evdev subsystem, maps keycodes to
//! human-readable names and logs key press/release/repeat events.
//!
//! Supports kernel `EVIOCGRAB` exclusive grabbing to suppress system desktop actions
//! and `/dev/uinput` virtual key remapping (e.g. TV button -> inject '1').

use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::*;
    use crate::virtual_keyboard::VirtualKeyboard;
    use evdev::{enumerate, Device, EventType, KeyCode};
    use std::path::PathBuf;

    const KNOWN_NAMES: &[&str] = &[
        "小米蓝牙语音遥控器",
        "MI RC",
        "Xiaomi Bluetooth Remote",
        "RC003",
        "ARN9",
    ];

    const TARGET_VENDOR: u16 = 0x2717;
    const TARGET_PRODUCT: u16 = 0x32b8;

    fn is_target_device(dev: &Device) -> bool {
        let id = dev.input_id();
        if id.vendor() == TARGET_VENDOR && id.product() == TARGET_PRODUCT {
            return true;
        }
        if let Some(name) = dev.name() {
            for known in KNOWN_NAMES {
                if name.contains(known) {
                    return true;
                }
            }
        }
        false
    }

    fn find_remote_device() -> Option<(PathBuf, String)> {
        for (path, dev) in enumerate() {
            if is_target_device(&dev) {
                let name = dev.name().unwrap_or("Xiaomi Remote").to_string();
                return Some((path, name));
            }
        }
        None
    }

    fn format_key_name(key: KeyCode) -> &'static str {
        match key {
            KeyCode::KEY_UP => "Up (上)",
            KeyCode::KEY_DOWN => "Down (下)",
            KeyCode::KEY_LEFT => "Left (左)",
            KeyCode::KEY_RIGHT => "Right (右)",
            KeyCode::KEY_ENTER => "OK / Select (确定)",
            KeyCode::KEY_OK => "OK / Select (确定)",
            KeyCode::KEY_BACK => "Back (返回)",
            KeyCode::KEY_ESC => "Back / Esc (返回)",
            KeyCode::KEY_HOMEPAGE => "Home (主页)",
            KeyCode::KEY_HOME => "Home (主页)",
            KeyCode::KEY_MENU => "Menu (菜单)",
            KeyCode::KEY_COMPOSE => "Menu / App (菜单)",
            KeyCode::KEY_TV => "TV (电视快捷键)",
            KeyCode::KEY_GRAVE => "TV / Backquote (`/TV)",
            KeyCode::KEY_VOLUMEUP => "Volume + (音量+)",
            KeyCode::KEY_VOLUMEDOWN => "Volume - (音量-)",
            KeyCode::KEY_MUTE => "Mute (静音)",
            KeyCode::KEY_POWER => "Power (电源)",
            KeyCode::KEY_F5 => "Voice / Mic (语音/麦克风 F5)",
            KeyCode::KEY_SEARCH => "Voice / Search (语音搜索)",
            KeyCode::KEY_MICMUTE => "Mic Mute (麦克风静音)",
            KeyCode::KEY_DELETE => "Delete (删除)",
            KeyCode::KEY_BACKSPACE => "Backspace (退格)",
            _ => "Other Button",
        }
    }

    pub async fn run_key_monitor(shutdown: CancellationToken, grab: bool) {
        log::info!(
            "[KEY MONITOR] Starting Remote Button Event Listener (System suppression grab: {})...",
            if grab { "ENABLED (独占拦截系统事件)" } else { "DISABLED (直通系统)" }
        );

        let mut vkeyboard = VirtualKeyboard::new();

        while !shutdown.is_cancelled() {
            let Some((path, name)) = find_remote_device() else {
                log::debug!("[KEY MONITOR] Remote input device not found in /dev/input/event*, waiting...");
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => continue,
                }
            };

            log::info!(
                "[KEY MONITOR] Found remote input device: \"{}\" at {}",
                name,
                path.display()
            );

            let Ok(mut device) = Device::open(&path) else {
                log::warn!(
                    "[KEY MONITOR] Failed to open device {}. Check permissions (user in 'input' group).",
                    path.display()
                );
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => continue,
                }
            };

            if grab {
                if let Err(e) = device.grab() {
                    log::warn!("[KEY MONITOR] Failed to grab device exclusively (EVIOCGRAB): {e}");
                } else {
                    log::info!(
                        "[KEY MONITOR] Device grabbed exclusively (EVIOCGRAB). System key events are suppressed (won't affect VSCode, volume, or OS shortcuts)."
                    );
                }
            }

            let Ok(mut stream) = device.into_event_stream() else {
                log::warn!("[KEY MONITOR] Failed to create event stream for {}", path.display());
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(3)) => continue,
                }
            };

            log::info!("[KEY MONITOR] Successfully listening for remote button events!");

            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    event_res = stream.next_event() => {
                        match event_res {
                            Ok(ev) => {
                                if ev.event_type() == EventType::KEY {
                                    let key = KeyCode::new(ev.code());
                                    let key_label = format_key_name(key);
                                    let state_str = match ev.value() {
                                        1 => "Pressed  (按下)",
                                        0 => "Released (松开)",
                                        2 => "Hold     (长按)",
                                        _ => "Unknown State",
                                    };

                                    log::info!(
                                        "[KEY EVENT] [{:<15}] Button: {:<25} (code: {})",
                                        state_str,
                                        key_label,
                                        ev.code()
                                    );

                                    // Action Mapping: TV Button -> Virtual Input '1'
                                    if ev.value() == 1 && (key == KeyCode::KEY_TV || key == KeyCode::KEY_GRAVE) {
                                        if vkeyboard.is_available() {
                                            if let Err(e) = vkeyboard.emit_key(KeyCode::KEY_1) {
                                                log::error!("[KEY REMAP] Failed to inject '1': {e}");
                                            } else {
                                                log::info!("[KEY REMAP] >> TV Button pressed -> Injected '1' into active window <<");
                                            }
                                        } else {
                                            log::warn!("[KEY REMAP] TV Button pressed, but /dev/uinput virtual keyboard is unavailable (check permissions).");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                log::warn!("[KEY MONITOR] Input event stream ended or error: {e}");
                                break;
                            }
                        }
                    }
                }
            }

            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }

        log::info!("[KEY MONITOR] Key monitor stopped.");
    }
}

#[cfg(not(target_os = "linux"))]
mod non_linux_impl {
    use super::*;

    pub async fn run_key_monitor(shutdown: CancellationToken, _grab: bool) {
        log::info!("[KEY MONITOR] evdev key monitoring is only supported on Linux.");
        shutdown.cancelled().await;
    }
}

pub async fn run_key_monitor(shutdown: CancellationToken, grab: bool) {
    #[cfg(target_os = "linux")]
    linux_impl::run_key_monitor(shutdown, grab).await;

    #[cfg(not(target_os = "linux"))]
    non_linux_impl::run_key_monitor(shutdown, grab).await;
}
