//! Virtual Keyboard Injector via Linux /dev/uinput.
//!
//! Emits hardware-level keystrokes to the Linux kernel, bypassing Wayland/X11
//! compositor limitations and typing into active window cursors.

#[cfg(target_os = "linux")]
mod linux_impl {
    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, EventType, InputEvent, KeyCode, SynchronizationCode};
    use std::sync::{Arc, Mutex};

    pub struct VirtualKeyboard {
        device: Option<VirtualDevice>,
    }

    impl VirtualKeyboard {
        pub fn new() -> Self {
            let mut keys = AttributeSet::<KeyCode>::new();
            // Insert all alphanumeric keys, navigation, function keys, and modifiers
            for code in 0..255 {
                keys.insert(KeyCode::new(code));
            }

            match VirtualDevice::builder()
                .and_then(|b| b.name("Xiaomi Remote Virtual Keyboard").with_keys(&keys))
                .and_then(|b| b.build())
            {
                Ok(dev) => {
                    log::info!("[VIRTUAL KEYBOARD] Initialized /dev/uinput virtual keyboard successfully.");
                    Self { device: Some(dev) }
                }
                Err(e) => {
                    log::warn!(
                        "[VIRTUAL KEYBOARD] Could not open /dev/uinput: {e}.\n\
                        ==> TIP: To enable virtual keyboard on Linux without root, run:\n\
                        ==> sudo chmod 0660 /dev/uinput && sudo chgrp input /dev/uinput\n\
                        ==> Or add udev rule: echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee /etc/udev/rules.d/99-uinput.rules"
                    );
                    Self { device: None }
                }
            }
        }

        pub fn is_available(&self) -> bool {
            self.device.is_some()
        }


        pub fn emit_key(&mut self, key: KeyCode) -> anyhow::Result<()> {
            let Some(dev) = self.device.as_mut() else {
                anyhow::bail!("/dev/uinput virtual keyboard is not available");
            };

            let syn = InputEvent::new(EventType::SYNCHRONIZATION.0, SynchronizationCode::SYN_REPORT.0, 0);
            let down = InputEvent::new(EventType::KEY.0, key.code(), 1);
            let up = InputEvent::new(EventType::KEY.0, key.code(), 0);

            // Press down
            dev.emit(&[down, syn])?;
            // Release up
            dev.emit(&[up, syn])?;

            log::info!("[VIRTUAL KEYBOARD] Injected key: {:?} (code: {})", key, key.code());
            Ok(())
        }

        pub fn emit_combo(&mut self, modifiers: &[KeyCode], key: KeyCode) -> anyhow::Result<()> {
            let Some(dev) = self.device.as_mut() else {
                anyhow::bail!("/dev/uinput virtual keyboard is not available");
            };

            let syn = InputEvent::new(EventType::SYNCHRONIZATION.0, SynchronizationCode::SYN_REPORT.0, 0);

            // 1. Modifiers down
            for &m in modifiers {
                dev.emit(&[InputEvent::new(EventType::KEY.0, m.code(), 1), syn])?;
            }

            // 2. Key down & up
            dev.emit(&[InputEvent::new(EventType::KEY.0, key.code(), 1), syn])?;
            dev.emit(&[InputEvent::new(EventType::KEY.0, key.code(), 0), syn])?;

            // 3. Modifiers up (reverse order)
            for &m in modifiers.iter().rev() {
                dev.emit(&[InputEvent::new(EventType::KEY.0, m.code(), 0), syn])?;
            }

            log::info!(
                "[VIRTUAL KEYBOARD] Injected combo: {:?} + {:?} (code: {})",
                modifiers,
                key,
                key.code()
            );
            Ok(())
        }
    }

    impl Default for VirtualKeyboard {
        fn default() -> Self {
            Self::new()
        }
    }

    pub type SharedVirtualKeyboard = Arc<Mutex<VirtualKeyboard>>;

    pub fn create_shared_virtual_keyboard() -> SharedVirtualKeyboard {
        Arc::new(Mutex::new(VirtualKeyboard::new()))
    }
}

#[cfg(not(target_os = "linux"))]
mod non_linux_impl {
    use std::sync::{Arc, Mutex};

    /// Stub keyboard for non-Linux targets: always unavailable, never injects.
    pub struct VirtualKeyboard;

    impl VirtualKeyboard {
        pub fn new() -> Self {
            Self
        }
        pub fn is_available(&self) -> bool {
            false
        }
    }

    impl Default for VirtualKeyboard {
        fn default() -> Self {
            Self::new()
        }
    }

    pub type SharedVirtualKeyboard = Arc<Mutex<VirtualKeyboard>>;

    pub fn create_shared_virtual_keyboard() -> SharedVirtualKeyboard {
        Arc::new(Mutex::new(VirtualKeyboard::new()))
    }
}

#[cfg(target_os = "linux")]
pub use linux_impl::*;

#[cfg(not(target_os = "linux"))]
pub use non_linux_impl::*;
