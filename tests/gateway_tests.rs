use xiaomi_remote_pro2_gateway::adpcm::ImaAdpcmDecoder;
use xiaomi_remote_pro2_gateway::atvv::{Caps, ControlEvent};
use xiaomi_remote_pro2_gateway::dsp::{calculate_metrics, FrameAccumulator};
use xiaomi_remote_pro2_gateway::recorder::{AudioSink, AutoWavRecorder};
use xiaomi_remote_pro2_gateway::session::{Action, AtvvSession};

#[test]
fn test_atvv_caps_parsing() {
    let raw = [0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78];
    let caps = Caps::parse(&raw).unwrap();
    assert_eq!(caps.version, 0x0100);
    assert_eq!(caps.codec, 0x02);
    assert_eq!(caps.frame_size, 120);
    assert_eq!(caps.sample_rate, 16_000);
}

#[test]
fn test_atvv_control_event_parsing() {
    assert_eq!(ControlEvent::parse(&[0x08]), Some(ControlEvent::StartSearch));
    assert_eq!(
        ControlEvent::parse(&[0x04, 0x00, 0x02, 0x05]),
        Some(ControlEvent::AudioStart {
            codec: Some(0x02),
            session_id: Some(5)
        })
    );
    assert_eq!(ControlEvent::parse(&[0x00]), Some(ControlEvent::AudioStop));
}

#[test]
fn test_adpcm_decoding() {
    let mut dec = ImaAdpcmDecoder::new();
    let mut out = Vec::new();
    dec.decode_into(&[0x77], &mut out);
    assert_eq!(out.len(), 2);
    assert_eq!(out, vec![11, 41]);
}

#[test]
fn test_dsp_frame_accumulator() {
    let mut acc = FrameAccumulator::new();
    let mut frames = Vec::new();
    acc.push(&[1, 2], 3, |f| frames.push(f.to_vec()));
    assert!(frames.is_empty());
    acc.push(&[3, 4, 5, 6], 3, |f| frames.push(f.to_vec()));
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0], vec![1, 2, 3]);
    assert_eq!(frames[1], vec![4, 5, 6]);
}

#[test]
fn test_dsp_metrics() {
    let samples = vec![16384, -16384, 16384, -16384];
    let metrics = calculate_metrics(&samples);
    assert_eq!(metrics.peak_sample, 16384);
    assert!((metrics.peak_dbfs - (-6.02)).abs() < 0.1);
}

#[test]
fn test_session_state_machine() {
    let mut s = AtvvSession::new(0.0);
    let mut actions = Vec::new();

    // Send Caps
    s.handle_control(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x01], 0, &mut actions);
    assert!(s.is_caps_confirmed());

    // Start Search
    s.handle_control(&[0x08], 10, &mut actions);
    assert_eq!(actions, vec![Action::SendTx(vec![0x0C, 0x00])]);
    actions.clear();

    // Audio Start
    s.handle_control(&[0x04, 0x00, 0x02, 0x01], 20, &mut actions);
    assert_eq!(actions, vec![Action::StreamStarted]);
    assert!(s.is_streaming());
    actions.clear();

    // Receive audio bytes
    s.handle_audio(&[0x77], 30, &mut actions);
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0], Action::Pcm(vec![11, 41]));
    actions.clear();

    // Audio Stop
    s.handle_control(&[0x00], 40, &mut actions);
    assert_eq!(actions, vec![Action::StreamStopped]);
    assert!(!s.is_streaming());
}

#[test]
fn test_wav_recorder() {
    let dir = std::env::temp_dir().join("gateway_test_records");
    let mut recorder = AutoWavRecorder::new(dir.clone(), None);

    recorder.on_stream_start();
    recorder.push(&[100, 200, 300, -100, -200, -300]);
    recorder.on_stream_stop();

    // Verify WAV file was created
    let files = std::fs::read_dir(&dir).unwrap();
    let count = files.count();
    assert!(count > 0);

    // Clean up
    let _ = std::fs::remove_dir_all(&dir);
}
