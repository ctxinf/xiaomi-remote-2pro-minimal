//! Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Voice Gateway.
//!
//! A headless gateway focused on the remote's microphone button:
//! - Receives ATVV voice events over BLE
//! - Decodes the stream and saves each take to a temporary WAV file
//! - Triggers hook scripts on the `start` / `end` recording events

pub mod adpcm;
pub mod atvv;
pub mod ble_bridge;
pub mod config;
pub mod dsp;
pub mod hooks;
pub mod recorder;
pub mod retention;
pub mod session;
pub mod voice_key;

pub use bluest;
pub use tokio_util;
