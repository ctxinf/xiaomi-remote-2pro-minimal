//! Audio Sink & WAV Recorder.
//!
//! Writes each voice session to its own timestamped WAV file in a temporary
//! directory, then fires the recording hooks:
//! - `start` when the voice stream opens,
//! - `end` when it closes **and** left a valid audio file behind.
//!
//! Hooks may move, upload, or remove recordings. Short takes and older files
//! are removed according to the gateway retention policy.

use crate::dsp::calculate_metrics;
use crate::hooks::{HookConfig, HookContext, HookEvent};
use crate::retention::{sweep, RetentionPolicy};
use chrono::Local;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

pub const SAMPLE_RATE: u32 = 16_000;

/// A recording shorter than this is treated as a stray keypress, not speech, and
/// does not produce an `end` hook.
const MIN_VALID_SAMPLES: u64 = SAMPLE_RATE as u64 / 20; // 50 ms

/// Default directory for recorded audio: `$TMPDIR/xiaomi-remote-gateway`.
pub fn default_tmp_dir() -> PathBuf {
    std::env::temp_dir().join("xiaomi-remote-gateway")
}

/// Audio Sink Trait for consuming 16 kHz 16-bit linear PCM streams.
pub trait AudioSink: Send {
    fn on_stream_start(&mut self);
    fn push(&mut self, pcm: &[i16]);
    fn on_stream_stop(&mut self);
}

/// Records each voice session to its own WAV file and fires hooks around it.
pub struct WavRecorder {
    dir: PathBuf,
    hooks: HookConfig,
    retention: RetentionPolicy,
    current: Option<Take>,
}

/// State of the recording currently in progress.
struct Take {
    writer: hound::WavWriter<BufWriter<File>>,
    path: PathBuf,
    samples: u64,
    peak: i16,
    sum_squared: f64,
}

impl WavRecorder {
    pub fn new(dir: PathBuf, hooks: HookConfig, retention: RetentionPolicy) -> Self {
        Self {
            dir,
            hooks,
            retention,
            current: None,
        }
    }

    /// Prunes recordings left behind by earlier runs. Call once at startup.
    pub fn sweep_startup(&self) {
        sweep(&self.dir, &self.retention, None);
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn open_take(&mut self) -> anyhow::Result<()> {
        if !self.dir.exists() {
            fs::create_dir_all(&self.dir).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create record directory {}: {e}",
                    self.dir.display()
                )
            })?;
        }

        // Two takes can land in the same millisecond, and the hook for the
        // first one may still be reading its file, so never reuse a name.
        let timestamp = Local::now().format("%Y%m%d_%H%M%S%.3f");
        let mut path = self.dir.join(format!("record_{timestamp}.wav"));
        for n in 1.. {
            if !path.exists() {
                break;
            }
            path = self.dir.join(format!("record_{timestamp}_{n}.wav"));
        }

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(&path, spec)
            .map_err(|e| anyhow::anyhow!("Failed to create WAV file at {}: {e}", path.display()))?;

        log::info!("[AUDIO RECORD] Started recording -> {}", path.display());

        self.current = Some(Take {
            writer,
            path,
            samples: 0,
            peak: 0,
            sum_squared: 0.0,
        });
        Ok(())
    }

    /// Finalizes the in-progress take and fires the `end` hook if it produced a
    /// valid audio file.
    fn finalize_take(&mut self) {
        let Some(take) = self.current.take() else {
            return;
        };
        let Take {
            writer,
            path,
            samples,
            peak,
            sum_squared,
        } = take;

        if let Err(e) = writer.finalize() {
            log::error!(
                "[AUDIO RECORD] Failed to finalize WAV file {}: {e}",
                path.display()
            );
            return;
        }

        let duration_sec = samples as f64 / SAMPLE_RATE as f64;
        let rms = if samples > 0 {
            (sum_squared / samples as f64).sqrt()
        } else {
            0.0
        };

        log::info!(
            "[AUDIO RECORD] Saved: {} | Duration: {:.2}s ({} samples) | Peak: {:.1} dBFS | RMS: {:.1} dBFS",
            path.display(),
            duration_sec,
            samples,
            dbfs(peak as f64 / 32768.0),
            dbfs(rms)
        );

        if samples < MIN_VALID_SAMPLES {
            // No `end` hook fires for these, so nothing downstream would ever
            // claim the file; drop it here rather than leaving an orphan.
            log::debug!(
                "[AUDIO RECORD] Take too short ({samples} samples), discarding and skipping \"end\" hook."
            );
            if let Err(e) = fs::remove_file(&path) {
                log::debug!(
                    "[AUDIO RECORD] Could not remove short take {}: {e}",
                    path.display()
                );
            }
            return;
        }

        self.hooks.fire(
            HookEvent::End,
            &HookContext {
                audio_file: Some(absolute(&path)),
                duration_sec,
                samples,
                sample_rate: SAMPLE_RATE,
            },
        );

        // Reap leftovers from previous takes. The file just handed to the hook
        // is excluded: the hook may still be reading it.
        sweep(&self.dir, &self.retention, Some(&path));
    }
}

/// Best-effort absolutization so hooks always receive a usable path, even when
/// the gateway was started with a relative `--record-dir`.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn dbfs(normalized: f64) -> f64 {
    if normalized > 1e-5 {
        20.0 * normalized.log10()
    } else {
        -96.0
    }
}

impl AudioSink for WavRecorder {
    fn on_stream_start(&mut self) {
        if self.current.is_some() {
            return;
        }
        if let Err(e) = self.open_take() {
            log::error!("[AUDIO RECORD] {e}");
            return;
        }
        self.hooks.fire(
            HookEvent::Start,
            &HookContext {
                sample_rate: SAMPLE_RATE,
                ..Default::default()
            },
        );
    }

    fn push(&mut self, pcm: &[i16]) {
        if pcm.is_empty() {
            return;
        }
        if self.current.is_none() {
            // Audio can arrive before AUDIO_START; open a take rather than drop it.
            self.on_stream_start();
        }
        let Some(take) = self.current.as_mut() else {
            return;
        };

        for &sample in pcm {
            if let Err(e) = take.writer.write_sample(sample) {
                log::error!("[AUDIO RECORD] Write error: {e}");
                self.current = None;
                return;
            }
            let abs_val = sample.saturating_abs();
            if abs_val > take.peak {
                take.peak = abs_val;
            }
            let norm = sample as f64 / 32768.0;
            take.sum_squared += norm * norm;
        }
        take.samples += pcm.len() as u64;

        if log::log_enabled!(log::Level::Debug) {
            let metrics = calculate_metrics(pcm);
            log::debug!(
                "[AUDIO STREAM] Chunk: {} samples | Instant Peak: {:.1} dBFS | Instant RMS: {:.1} dBFS",
                pcm.len(),
                metrics.peak_dbfs,
                metrics.rms_dbfs
            );
        }
    }

    fn on_stream_stop(&mut self) {
        self.finalize_take();
    }
}

impl Drop for WavRecorder {
    fn drop(&mut self) {
        self.finalize_take();
    }
}
