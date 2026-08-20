//! Xiaomi Bluetooth Remote 2 Pro (RC003/ARN9) Rust Gateway.
//!
//! A high-performance, non-GUI headless gateway for Xiaomi Bluetooth Voice Remote:
//! - Real-time button event monitoring, grabbing & virtual keyboard remapping
//! - Automatic 16 kHz voice recording to timestamped WAV files
//! - Connection, battery, and ATVV protocol status reporting

pub mod adpcm;
pub mod atvv;
pub mod ble_bridge;
pub mod dsp;
pub mod key_monitor;
pub mod recorder;
pub mod session;
pub mod virtual_keyboard;

pub use bluest;
pub use tokio_util;
