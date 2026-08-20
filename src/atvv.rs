//! Android TV Voice-over-BLE (ATVV) Protocol Constants and Packet Parsing.
//!
//! Xiaomi Bluetooth Remote 2 Pro (RC003) and standard RC001 / ARN9 use Google's
//! ATVV private GATT protocol for voice transmission over BLE:
//!
//! ```text
//! Host (Gateway)                       Remote (RC003)
//!   |--- TX: GET_CAPS (0x0A) ------------->|
//!   |<-- CTL: CAPS_RESP (0x0B) ------------|  Version, codec, frame size
//!   |<-- CTL: START_SEARCH (0x08) ---------|  User presses Mic button
//!   |--- TX: MIC_OPEN (0x0C) ------------->|
//!   |<-- CTL: AUDIO_START (0x04) ----------|
//!   |<-- AUDIO: ADPCM frame stream (notify)|  16 kHz IMA ADPCM
//!   |<-- CTL: AUDIO_SYNC (0x0A) -----------|  Sync predictor & step index on packet loss
//!   |<-- CTL: AUDIO_STOP (0x00) -----------|  User releases Mic button
//!   |--- TX: MIC_CLOSE (0x0D) ------------>|
//! ```

use uuid::Uuid;

/// ATVV Voice Service UUID.
pub const SERVICE: Uuid = Uuid::from_u128(0xAB5E0001_5A21_4F05_BC7D_AF01F617B664);

/// Host -> Remote command channel (write).
pub const CHAR_TX: Uuid = Uuid::from_u128(0xAB5E0002_5A21_4F05_BC7D_AF01F617B664);

/// Remote -> Host audio stream channel (notify).
pub const CHAR_AUDIO: Uuid = Uuid::from_u128(0xAB5E0003_5A21_4F05_BC7D_AF01F617B664);

/// Remote -> Host control event channel (notify).
pub const CHAR_CTL: Uuid = Uuid::from_u128(0xAB5E0004_5A21_4F05_BC7D_AF01F617B664);

/// Converts 16-bit standard Bluetooth UUID to 128-bit Bluetooth Base UUID.
pub const fn bt16(short: u16) -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_1000_8000_0080_5F9B_34FB | ((short as u128) << 96))
}

/// Device Information Service (0x180A)
pub const DEVICE_INFORMATION_SERVICE: Uuid = bt16(0x180A);
/// Model Number String (0x2A24)
pub const MODEL_NUMBER_CHARACTERISTIC: Uuid = bt16(0x2A24);
/// Firmware Revision String (0x2A26)
pub const FIRMWARE_REVISION_CHARACTERISTIC: Uuid = bt16(0x2A26);
/// Hardware Revision String (0x2A27)
pub const HARDWARE_REVISION_CHARACTERISTIC: Uuid = bt16(0x2A27);
/// Manufacturer Name String (0x2A29)
pub const MANUFACTURER_NAME_CHARACTERISTIC: Uuid = bt16(0x2A29);

/// Battery Service (0x180F)
pub const BATTERY_SERVICE: Uuid = bt16(0x180F);
/// Battery Level Characteristic (0x2A19)
pub const BATTERY_LEVEL_CHARACTERISTIC: Uuid = bt16(0x2A19);

/// GET_CAPS command (v1.0 layout): Host requests capabilities and declares support for 8k/16k ADPCM.
pub const GET_CAPS: [u8; 6] = [0x0A, 0x01, 0x00, 0x00, 0x03, 0x03];

/// Builds MIC_OPEN command.
pub fn mic_open(version: u16, codec: u8) -> Vec<u8> {
    if version >= 0x0100 {
        vec![0x0C, 0x00]
    } else {
        vec![0x0C, 0x00, codec]
    }
}

/// Builds MIC_CLOSE command.
pub fn mic_close(version: u16, session_id: u8) -> Vec<u8> {
    if version >= 0x0100 {
        vec![0x0D, session_id]
    } else {
        vec![0x0D]
    }
}

/// Parsed ATVV Capabilities from 0x0B (CAPS_RESP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub version: u16,
    pub codecs: u8,
    /// Number of bytes in one ADPCM frame.
    pub frame_size: usize,
    /// 0x01 = 8 kHz, 0x02 = 16 kHz.
    pub codec: u8,
    pub sample_rate: u32,
}

impl Caps {
    /// Default values for Xiaomi RC003: 16 kHz ADPCM, 120 bytes per frame.
    pub const DEFAULT: Caps = Caps {
        version: 0x0100,
        codecs: 0x02,
        frame_size: 120,
        codec: 0x02,
        sample_rate: 16_000,
    };

    /// Parse 0x0B CAPS_RESP payload.
    pub fn parse(data: &[u8]) -> Option<Caps> {
        if data.len() < 7 || data[0] != 0x0B {
            return None;
        }
        let version = u16::from_be_bytes([data[1], data[2]]);
        let codecs = if version >= 0x0100 {
            let mut c = data[3];
            if c == 0 && data.len() >= 9 && data[4] & 0x03 != 0 {
                c = data[4];
            }
            c
        } else {
            if data.len() < 9 {
                return None;
            }
            data[4]
        };
        let frame_size = ((data[5] as usize) << 8) | data[6] as usize;
        let codec: u8 = if codecs & 0x02 != 0 { 0x02 } else { 0x01 };
        Some(Caps {
            version,
            codecs,
            frame_size: if frame_size == 0 { 120 } else { frame_size },
            codec,
            sample_rate: if codec == 0x02 { 16_000 } else { 8_000 },
        })
    }

    /// Update codec and sample rate based on AUDIO_START payload.
    pub fn with_stream_codec(self, codec: u8) -> Caps {
        Caps {
            codec,
            sample_rate: if codec == 0x02 { 16_000 } else { 8_000 },
            ..self
        }
    }
}

/// Control event received on CHAR_CTL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// 0x0B: Capabilities response.
    CapsResp,
    /// 0x08: Remote requests opening microphone (Mic button pressed).
    StartSearch,
    /// 0x04: Audio stream starting.
    AudioStart { codec: Option<u8>, session_id: Option<u8> },
    /// 0x00: Audio stream stopped (Mic button released).
    AudioStop,
    /// 0x0A: Packet sync containing decoder predictor and step index.
    AudioSync { predictor: i16, step_index: u8 },
    /// Other / unrecognized opcode.
    Other(u8),
}

impl ControlEvent {
    pub fn parse(data: &[u8]) -> Option<ControlEvent> {
        match *data.first()? {
            0x0B => Some(Self::CapsResp),
            0x08 => Some(Self::StartSearch),
            0x04 => Some(Self::AudioStart {
                codec: data.get(2).copied(),
                session_id: data.get(3).copied(),
            }),
            0x00 => Some(Self::AudioStop),
            0x0A => {
                if data.len() < 7 {
                    return None;
                }
                Some(Self::AudioSync {
                    predictor: i16::from_be_bytes([data[4], data[5]]),
                    step_index: data[6],
                })
            }
            op => Some(Self::Other(op)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_caps_parse() {
        let raw = [0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78];
        let caps = Caps::parse(&raw).expect("valid caps");
        assert_eq!(caps.version, 0x0100);
        assert_eq!(caps.codec, 0x02);
        assert_eq!(caps.sample_rate, 16_000);
        assert_eq!(caps.frame_size, 120);
    }

    #[test]
    fn test_control_event_parse() {
        assert_eq!(ControlEvent::parse(&[0x08]), Some(ControlEvent::StartSearch));
        assert_eq!(
            ControlEvent::parse(&[0x04, 0x00, 0x02, 0x01]),
            Some(ControlEvent::AudioStart {
                codec: Some(0x02),
                session_id: Some(1)
            })
        );
        assert_eq!(ControlEvent::parse(&[0x00]), Some(ControlEvent::AudioStop));
        assert_eq!(
            ControlEvent::parse(&[0x0A, 0, 0, 0, 0x01, 0x00, 0x05]),
            Some(ControlEvent::AudioSync {
                predictor: 256,
                step_index: 5
            })
        );
    }
}
