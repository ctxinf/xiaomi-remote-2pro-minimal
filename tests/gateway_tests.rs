use std::sync::OnceLock;
use xiaomi_remote_pro2_gateway::adpcm::ImaAdpcmDecoder;
use xiaomi_remote_pro2_gateway::atvv::{Caps, ControlEvent};
use xiaomi_remote_pro2_gateway::dsp::{calculate_metrics, FrameAccumulator};
use xiaomi_remote_pro2_gateway::hooks::{HookConfig, HookEvent};
use xiaomi_remote_pro2_gateway::recorder::{AudioSink, WavRecorder};
use xiaomi_remote_pro2_gateway::retention::{sweep, RetentionPolicy};
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
    assert_eq!(actions, vec![Action::StreamStopped, Action::SendTx(vec![0x0D, 1])]);
    assert!(!s.is_streaming());
}

#[test]
fn test_wav_recorder_writes_one_file_per_session() {
    let dir = std::env::temp_dir().join("gateway_test_records");
    let _ = std::fs::remove_dir_all(&dir);
    let mut recorder = WavRecorder::new(dir.clone(), HookConfig::new(), RetentionPolicy::from_args(0, 0));

    recorder.on_stream_start();
    recorder.push(&[1000i16; 1600]);
    recorder.on_stream_stop();

    recorder.on_stream_start();
    recorder.push(&[1000i16; 1600]);
    recorder.on_stream_stop();

    let wavs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
        .collect();
    assert_eq!(wavs.len(), 2, "each session gets its own file");
    for wav in &wavs {
        assert!(wav.metadata().unwrap().len() > 44, "wav has audio past the header");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `end` hook must receive the finished audio file, since that is the whole
/// point of the hook system.
#[test]
fn test_end_hook_receives_audio_file() {
    let dir = std::env::temp_dir().join("gateway_test_end_hook");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let marker = dir.join("hook_output.txt");
    let script = write_hook_script(&dir, &marker);

    let mut hooks = HookConfig::new();
    hooks.add(HookEvent::End, script);

    let mut recorder = WavRecorder::new(dir.clone(), hooks, RetentionPolicy::from_args(0, 0));
    recorder.on_stream_start();
    recorder.push(&[1000i16; 1600]); // 100 ms, above the minimum
    recorder.on_stream_stop();

    let output = wait_for_file(&marker).expect("end hook should have run");
    assert!(output.contains("\"event\":\"end\""), "payload: {output}");
    assert!(output.contains(".wav"), "hook should receive the audio path: {output}");

    let audio_path = output
        .lines()
        .find_map(|l| l.strip_prefix("ENV_FILE="))
        .expect("hook should get XIAOMI_AUDIO_FILE");
    assert!(
        std::path::Path::new(audio_path.trim()).exists(),
        "audio file {audio_path} must still exist for the hook to consume"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A take too short to be speech should not fire the `end` hook.
#[test]
fn test_short_take_does_not_fire_end_hook() {
    let dir = std::env::temp_dir().join("gateway_test_short_take");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let marker = dir.join("hook_output.txt");
    let script = write_hook_script(&dir, &marker);

    let mut hooks = HookConfig::new();
    hooks.add(HookEvent::End, script);

    let mut recorder = WavRecorder::new(dir.clone(), hooks, RetentionPolicy::from_args(0, 0));
    recorder.on_stream_start();
    recorder.push(&[1000i16; 10]);
    recorder.on_stream_stop();

    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!marker.exists(), "sub-50ms take must not trigger the end hook");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_hook_config_from_json_file() {
    let dir = std::env::temp_dir().join("gateway_test_hook_config");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let config_path = dir.join("hooks.json");
    std::fs::write(
        &config_path,
        r#"{"hooks":{"start":"./begin.sh","end":["./a.py","/abs/b.js"]}}"#,
    )
    .unwrap();

    let config = HookConfig::from_file(&config_path).unwrap();
    // Relative paths resolve against the config file's directory; absolute ones pass through.
    assert_eq!(config.scripts_for(HookEvent::Start), &[dir.join("begin.sh")]);
    assert_eq!(
        config.scripts_for(HookEvent::End),
        &[dir.join("a.py"), std::path::PathBuf::from("/abs/b.js")]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A hook given as a path relative to the working directory must still run:
/// the spawner sets the child's cwd, which previously broke relative lookup.
#[test]
fn test_relative_hook_script_path_runs() {
    let dir = std::env::temp_dir().join("gateway_test_relative_hook");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let marker = dir.join("hook_output.txt");
    let abs_script = write_hook_script(&dir, &marker);

    // Reference the script the way a user would on the command line.
    let cwd = std::env::current_dir().unwrap();
    let relative = pathdiff(&abs_script, &cwd);

    let mut hooks = HookConfig::new();
    hooks.add(HookEvent::End, relative);

    let mut recorder = WavRecorder::new(dir.clone(), hooks, RetentionPolicy::from_args(0, 0));
    recorder.on_stream_start();
    recorder.push(&[1000i16; 1600]);
    recorder.on_stream_stop();

    let output = wait_for_file(&marker).expect("relative-path hook should have run");
    assert!(output.contains("\"event\":\"end\""), "payload: {output}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Expresses `target` relative to `base` (both absolute) as `../..`-style path.
fn pathdiff(target: &std::path::Path, base: &std::path::Path) -> std::path::PathBuf {
    let mut up = std::path::PathBuf::new();
    let mut base = base.to_path_buf();
    loop {
        if let Ok(rest) = target.strip_prefix(&base) {
            return up.join(rest);
        }
        if !base.pop() {
            return target.to_path_buf();
        }
        up.push("..");
    }
}

/// A hook's own stdout/stderr must reach the gateway log rather than /dev/null,
/// otherwise scripts appear to run silently.
#[test]
fn test_hook_output_is_captured_not_discarded() {
    use std::sync::{Arc, Mutex};

    // Capture what the `log` facade actually emits.
    static LOGGER: OnceLock<CaptureLogger> = OnceLock::new();
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let logger = LOGGER.get_or_init(|| CaptureLogger {
        lines: Arc::new(Mutex::new(Vec::new())),
    });
    // Installing may fail if another test already set a logger; either way the
    // shared buffer below is what we assert on.
    let _ = log::set_logger(logger);
    log::set_max_level(log::LevelFilter::Info);
    logger.lines.lock().unwrap().clear();
    let _ = lines;

    let dir = std::env::temp_dir().join("gateway_test_hook_output");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let script = dir.join("noisy.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat > /dev/null\necho \"hello from stdout\"\necho \"uh oh\" >&2\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut hooks = HookConfig::new();
    hooks.add(HookEvent::End, script);

    let mut recorder = WavRecorder::new(dir.clone(), hooks, RetentionPolicy::from_args(0, 0));
    recorder.on_stream_start();
    recorder.push(&[1000i16; 1600]);
    recorder.on_stream_stop();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let (mut saw_stdout, mut saw_stderr) = (false, false);
    while std::time::Instant::now() < deadline && !(saw_stdout && saw_stderr) {
        std::thread::sleep(std::time::Duration::from_millis(40));
        let captured = logger.lines.lock().unwrap();
        saw_stdout = captured.iter().any(|l| l.contains("hello from stdout"));
        saw_stderr = captured.iter().any(|l| l.contains("uh oh"));
    }

    assert!(saw_stdout, "hook stdout must be relayed to the log");
    assert!(saw_stderr, "hook stderr must be relayed to the log");

    let _ = std::fs::remove_dir_all(&dir);
}

struct CaptureLogger {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl log::Log for CaptureLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, record: &log::Record) {
        self.lines.lock().unwrap().push(format!("{}", record.args()));
    }
    fn flush(&self) {}
}

/// Writes an executable hook that dumps its stdin payload and env to `marker`.
fn write_hook_script(dir: &std::path::Path, marker: &std::path::Path) -> std::path::PathBuf {
    let script = dir.join("hook.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n{{ cat; echo; echo \"ENV_FILE=$XIAOMI_AUDIO_FILE\"; }} > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

/// Hooks run detached, so poll briefly for the script's output.
fn wait_for_file(path: &std::path::Path) -> Option<String> {
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains("ENV_FILE=") {
                return Some(text);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
    None
}

#[test]
fn test_trailing_partial_frame_is_flushed_on_stop() {
    // frame_size = 120 bytes; feed 100 so nothing forms a complete frame.
    let mut s = AtvvSession::new(0.0);
    let mut actions = Vec::new();
    s.handle_control(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78], 0, &mut actions);
    s.handle_control(&[0x04, 0x00, 0x02, 0x01], 10, &mut actions);
    actions.clear();

    s.handle_audio(&[0x11; 100], 20, &mut actions);
    assert!(actions.is_empty(), "partial frame should not emit yet");

    s.handle_control(&[0x00], 30, &mut actions);
    match actions.as_slice() {
        [Action::Pcm(samples), Action::StreamStopped, Action::SendTx(close)] => {
            assert_eq!(close, &[0x0D, 1]);
            assert_eq!(samples.len(), 200, "100 bytes -> 200 PCM samples");
        }
        other => panic!("expected flushed tail then stop, got {other:?}"),
    }
}

#[test]
fn test_finish_flushes_partial_frame() {
    let mut s = AtvvSession::new(0.0);
    let mut actions = Vec::new();
    s.handle_control(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x78], 0, &mut actions);
    s.handle_control(&[0x04, 0x00, 0x02, 0x01], 10, &mut actions);
    actions.clear();

    s.handle_audio(&[0x11; 60], 20, &mut actions);
    actions.clear();

    s.finish(&mut actions);
    match actions.as_slice() {
        [Action::Pcm(samples), Action::StreamStopped] => assert_eq!(samples.len(), 120),
        other => panic!("expected flushed tail then stop, got {other:?}"),
    }
}

#[test]
fn test_empty_audio_packet_does_not_start_stream() {
    let mut s = AtvvSession::new(0.0);
    let mut actions = Vec::new();
    s.handle_control(&[0x0B, 0x01, 0x00, 0x02, 0x03, 0x00, 0x01], 0, &mut actions);
    actions.clear();

    s.handle_audio(&[], 100, &mut actions);
    assert!(actions.is_empty(), "empty notification must not open a stream");
    assert!(!s.is_streaming());
}


/// Count-based pruning keeps the newest files and drops the surplus.
#[test]
fn test_retention_keeps_newest_by_count() {
    let dir = std::env::temp_dir().join("gateway_test_retention_count");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Distinct mtimes so "newest" is unambiguous.
    for i in 0..5 {
        let path = dir.join(format!("record_2026010{i}_000000.000.wav"));
        std::fs::write(&path, b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    let deleted = sweep(&dir, &RetentionPolicy::from_args(0, 2), None);
    assert_eq!(deleted, 3);

    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left.len(), 2, "only the 2 newest survive: {left:?}");
    assert!(left.contains(&"record_20260104_000000.000.wav".to_string()));
    assert!(left.contains(&"record_20260103_000000.000.wav".to_string()));

    let _ = std::fs::remove_dir_all(&dir);
}

/// The sweep must not touch files it did not create, nor the protected file.
#[test]
fn test_retention_only_touches_own_files_and_respects_keep() {
    let dir = std::env::temp_dir().join("gateway_test_retention_scope");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let foreign = dir.join("important_notes.txt");
    let other_wav = dir.join("my_song.wav");
    std::fs::write(&foreign, b"do not delete").unwrap();
    std::fs::write(&other_wav, b"do not delete").unwrap();
    for i in 0..3 {
        std::fs::write(dir.join(format!("record_2026010{i}_000000.000.wav")), b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let protected = dir.join("record_20260100_000000.000.wav");

    // Limit of 1 would normally delete the two oldest, but one is protected.
    sweep(&dir, &RetentionPolicy::from_args(0, 1), Some(&protected));

    assert!(foreign.exists(), "non-recording files must never be deleted");
    assert!(other_wav.exists(), "unrelated .wav files must never be deleted");
    assert!(protected.exists(), "the in-flight file must be preserved for its hook");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Age-based pruning removes stale files and keeps fresh ones.
#[test]
fn test_retention_deletes_by_age() {
    let dir = std::env::temp_dir().join("gateway_test_retention_age");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let fresh = dir.join("record_20260101_000000.000.wav");
    std::fs::write(&fresh, b"x").unwrap();

    // A policy of 0 hours is "unlimited", so use a 1-hour window and rely on the
    // fresh file being well under it.
    let deleted = sweep(&dir, &RetentionPolicy::from_args(1, 0), None);
    assert_eq!(deleted, 0, "a just-written file is not expired");
    assert!(fresh.exists());

    let _ = std::fs::remove_dir_all(&dir);
}

/// A take too short to fire the `end` hook must not leave an orphan file.
#[test]
fn test_short_take_file_is_discarded() {
    let dir = std::env::temp_dir().join("gateway_test_short_orphan");
    let _ = std::fs::remove_dir_all(&dir);

    let mut recorder =
        WavRecorder::new(dir.clone(), HookConfig::new(), RetentionPolicy::from_args(0, 0));
    recorder.on_stream_start();
    recorder.push(&[1000i16; 10]); // well under the 50 ms threshold
    recorder.on_stream_stop();

    let wavs = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "wav")).count())
        .unwrap_or(0);
    assert_eq!(wavs, 0, "no hook can ever claim a short take, so it must be removed");

    let _ = std::fs::remove_dir_all(&dir);
}


#[test]
fn repeated_start_does_not_reset_decoder_or_lose_partial_audio() {
    let mut baseline = AtvvSession::new(0.0);
    let mut repeated = AtvvSession::new(0.0);
    let mut expected = Vec::new();
    let mut actual = Vec::new();
    for session in [&mut baseline, &mut repeated] {
        let mut setup = Vec::new();
        session.handle_control(&[0x0B, 1, 0, 2, 3, 0, 120], 0, &mut setup);
        session.handle_control(&[0x04, 0, 2, 1], 1, &mut setup);
    }
    baseline.handle_audio(&[0x77; 125], 2, &mut expected);
    repeated.handle_audio(&[0x77; 125], 2, &mut actual);
    repeated.handle_control(&[0x04, 0, 2, 1], 3, &mut actual);
    baseline.handle_audio(&[0x11; 115], 4, &mut expected);
    repeated.handle_audio(&[0x11; 115], 4, &mut actual);
    baseline.handle_control(&[0], 5, &mut expected);
    repeated.handle_control(&[0], 5, &mut actual);
    assert_eq!(
        actual, expected,
        "repeated start must preserve ADPCM state and pending frame"
    );
    actual.clear();
    repeated.handle_control(&[0], 6, &mut actual);
    assert!(
        actual.is_empty(),
        "repeated stop must not emit another MIC_CLOSE"
    );
}

#[test]
fn explicit_start_after_implicit_audio_preserves_decoder() {
    let mut session = AtvvSession::new(0.0);
    let mut actions = Vec::new();
    session.handle_control(&[0x0B, 1, 0, 2, 3, 0, 1], 0, &mut actions);
    session.handle_audio(&[0x77], 500, &mut actions);
    actions.clear();
    session.handle_control(&[0x04, 0, 2, 42], 501, &mut actions);
    assert!(actions.is_empty());
    session.handle_audio(&[0x77], 502, &mut actions);
    let mut decoder = ImaAdpcmDecoder::new();
    let mut pcm = Vec::new();
    decoder.decode_into(&[0x77, 0x77], &mut pcm);
    assert_eq!(actions, vec![Action::Pcm(pcm[2..].to_vec())]);
}

#[test]
fn full_negative_scale_metrics_do_not_overflow() {
    let metrics = calculate_metrics(&[i16::MIN]);
    assert!(metrics.peak_dbfs > -0.01);
    assert!(metrics.rms_dbfs.abs() < 0.01);
}
