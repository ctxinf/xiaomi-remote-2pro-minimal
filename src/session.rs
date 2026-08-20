//! ATVV Session State Machine.
//!
//! Manages connection-level state for one BLE connection session:
//! - Caps negotiation (GET_CAPS -> CAPS_RESP)
//! - Microphone opening (START_SEARCH -> MIC_OPEN -> AUDIO_START)
//! - Audio decoding (AUDIO notify -> IMA ADPCM -> PCM -> Action::Pcm)
//! - Loss recovery (AUDIO_SYNC -> reset decoder predictor & step)
//! - Microphone closing (AUDIO_STOP -> Action::StreamStopped -> MIC_CLOSE)

use crate::adpcm::ImaAdpcmDecoder;
use crate::atvv::{self, Caps, ControlEvent};
use crate::dsp::{postprocess, FrameAccumulator};

const STALE_AUDIO_WINDOW_MS: u64 = 300;

/// Action emitted by session to the outer bridge runner.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Write payload to ATVV TX characteristic.
    SendTx(Vec<u8>),
    /// Voice stream started.
    StreamStarted,
    /// Decoded 16 kHz 16-bit linear PCM samples.
    Pcm(Vec<i16>),
    /// Voice stream stopped.
    StreamStopped,
    /// Fatal protocol error that requires reconnection.
    Fatal(String),
}

pub struct AtvvSession {
    caps: Caps,
    caps_confirmed: bool,
    streaming: bool,
    mic_opened: bool,
    session_id: u8,
    last_stop_ms: Option<u64>,
    decoder: ImaAdpcmDecoder,
    frames: FrameAccumulator,
    pending_sync: Option<(i16, u8)>,
    gain_db: f64,
}

impl AtvvSession {
    pub fn new(gain_db: f64) -> Self {
        Self {
            caps: Caps::DEFAULT,
            caps_confirmed: false,
            streaming: false,
            mic_opened: false,
            session_id: 0,
            last_stop_ms: None,
            decoder: ImaAdpcmDecoder::new(),
            frames: FrameAccumulator::new(),
            pending_sync: None,
            gain_db,
        }
    }

    pub fn caps(&self) -> &Caps {
        &self.caps
    }

    pub fn is_caps_confirmed(&self) -> bool {
        self.caps_confirmed
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub fn take_mic_close(&mut self) -> Option<Vec<u8>> {
        if !self.mic_opened {
            return None;
        }
        self.mic_opened = false;
        Some(atvv::mic_close(self.caps.version, self.session_id))
    }

    pub fn handle_control(&mut self, data: &[u8], now_ms: u64, actions: &mut Vec<Action>) {
        let Some(event) = ControlEvent::parse(data) else {
            log::debug!("Unrecognized control packet: {data:02X?}");
            return;
        };
        match event {
            ControlEvent::CapsResp => {
                let Some(caps) = Caps::parse(data) else {
                    actions.push(Action::Fatal("Invalid ATVV CAPS_RESP from device".into()));
                    return;
                };
                if caps.sample_rate != 16_000 {
                    actions.push(Action::Fatal(format!(
                        "Unsupported sample rate {} Hz (requires 16 kHz)",
                        caps.sample_rate
                    )));
                    return;
                }
                log::info!(
                    "ATVV Capabilities Confirmed: Version={:#06X}, Codec={:#04X}, FrameSize={}B, Rate={}Hz",
                    caps.version,
                    caps.codec,
                    caps.frame_size,
                    caps.sample_rate
                );
                self.caps = caps;
                self.caps_confirmed = true;
            }
            ControlEvent::StartSearch => {
                if !self.caps_confirmed {
                    log::warn!("Received START_SEARCH before capabilities confirmed");
                    return;
                }
                log::info!("[REMOTE] Voice search button pressed (START_SEARCH), sending MIC_OPEN");
                self.mic_opened = true;
                actions.push(Action::SendTx(atvv::mic_open(
                    self.caps.version,
                    self.caps.codec,
                )));
            }
            ControlEvent::AudioStart { codec, session_id } => {
                if !self.caps_confirmed {
                    log::warn!("Received AUDIO_START before capabilities confirmed");
                    return;
                }
                if let Some(codec) = codec {
                    self.caps = self.caps.with_stream_codec(codec);
                }
                if self.caps.sample_rate != 16_000 {
                    actions.push(Action::Fatal(
                        "Remote switched to unsupported 8 kHz codec".into(),
                    ));
                    return;
                }
                self.session_id = session_id.unwrap_or(0);
                self.start_streaming(actions);
            }
            ControlEvent::AudioStop => {
                log::info!("[REMOTE] Voice stream stop received (AUDIO_STOP)");
                self.stop_streaming(now_ms, actions);
            }
            ControlEvent::AudioSync {
                predictor,
                step_index,
            } => {
                log::debug!(
                    "Audio sync received: predictor={}, step_index={}",
                    predictor,
                    step_index
                );
                self.pending_sync = Some((predictor, step_index));
                self.frames.reset();
            }
            ControlEvent::Other(op) => {
                log::debug!("Ignored custom control opcode: {op:#04X}");
            }
        }
    }

    pub fn handle_audio(&mut self, data: &[u8], now_ms: u64, actions: &mut Vec<Action>) {
        if !self.caps_confirmed || data.is_empty() {
            return;
        }
        if !self.streaming {
            if let Some(t) = self.last_stop_ms {
                if now_ms.saturating_sub(t) < STALE_AUDIO_WINDOW_MS {
                    return;
                }
            }
            log::debug!("Audio data arrived ahead of AUDIO_START, opening stream implicitly");
            self.start_streaming(actions);
        }

        let frame_size = self.caps.frame_size;
        let gain_db = self.gain_db;
        let mut pcm = Vec::new();
        let Self {
            frames,
            decoder,
            pending_sync,
            ..
        } = self;

        frames.push(data, frame_size, |frame| {
            if let Some((predictor, step_index)) = pending_sync.take() {
                decoder.reset(predictor as i32, step_index as i32);
            }
            let mut samples = Vec::with_capacity(frame.len() * 2);
            decoder.decode_into(frame, &mut samples);
            postprocess(&mut samples, gain_db);
            pcm.extend_from_slice(&samples);
        });

        if !pcm.is_empty() {
            actions.push(Action::Pcm(pcm));
        }
    }

    pub fn finish(&mut self, actions: &mut Vec<Action>) {
        if self.streaming {
            self.streaming = false;
            self.flush_partial(actions);
            actions.push(Action::StreamStopped);
        }
    }

    /// Decodes the trailing partial ADPCM frame, if any, before the stream ends.
    ///
    /// The remote cuts the stream mid-frame on mic release; without this the last
    /// (up to `frame_size - 1`) bytes of every take would be dropped.
    fn flush_partial(&mut self, actions: &mut Vec<Action>) {
        let partial = self.frames.take_partial();
        if partial.is_empty() {
            return;
        }
        if let Some((predictor, step_index)) = self.pending_sync.take() {
            self.decoder.reset(predictor as i32, step_index as i32);
        }
        let mut samples = Vec::with_capacity(partial.len() * 2);
        self.decoder.decode_into(&partial, &mut samples);
        postprocess(&mut samples, self.gain_db);
        if !samples.is_empty() {
            actions.push(Action::Pcm(samples));
        }
    }

    fn start_streaming(&mut self, actions: &mut Vec<Action>) {
        self.frames.reset();
        self.pending_sync = None;
        self.decoder.reset(0, 0);
        self.last_stop_ms = None;
        if !self.streaming {
            self.streaming = true;
            actions.push(Action::StreamStarted);
        }
    }

    fn stop_streaming(&mut self, now_ms: u64, actions: &mut Vec<Action>) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        self.flush_partial(actions);
        self.frames.reset();
        self.pending_sync = None;
        self.last_stop_ms = Some(now_ms);
        actions.push(Action::StreamStopped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS_16K: [u8; 7] = [0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x01];

    #[test]
    fn test_session_lifecycle() {
        let mut s = AtvvSession::new(0.0);
        let mut actions = Vec::new();

        // 1. Caps Resp
        s.handle_control(&CAPS_16K, 0, &mut actions);
        assert!(s.is_caps_confirmed());
        assert_eq!(actions.len(), 0);

        // 2. Start Search
        s.handle_control(&[0x08], 10, &mut actions);
        assert_eq!(actions, vec![Action::SendTx(vec![0x0C, 0x00])]);
        actions.clear();

        // 3. Audio Start
        s.handle_control(&[0x04, 0x00, 0x02, 0x01], 20, &mut actions);
        assert_eq!(actions, vec![Action::StreamStarted]);
        assert!(s.is_streaming());
        actions.clear();

        // 4. Audio frame
        s.handle_audio(&[0x77], 30, &mut actions);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::Pcm(vec![11, 41]));
        actions.clear();

        // 5. Audio Stop
        s.handle_control(&[0x00], 40, &mut actions);
        assert_eq!(actions, vec![Action::StreamStopped]);
        assert!(!s.is_streaming());
    }
}
