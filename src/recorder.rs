//! Audio Sink & WAV Recorder.
//!
//! Handles real-time PCM audio streaming, creating timestamped WAV files for each
//! voice session and printing detailed audio metrics upon completion.

use crate::dsp::calculate_metrics;
use chrono::Local;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;
use std::time::Instant;

/// Audio Sink Trait for consuming 16 kHz 16-bit linear PCM streams.
pub trait AudioSink: Send {
    fn on_stream_start(&mut self);
    fn push(&mut self, pcm: &[i16]);
    fn on_stream_stop(&mut self);
}

/// Auto-WAV Recorder that creates a new WAV file for each voice session.
pub struct AutoWavRecorder {
    record_dir: PathBuf,
    fixed_path: Option<PathBuf>,
    current_writer: Option<hound::WavWriter<BufWriter<File>>>,
    current_file_path: Option<PathBuf>,
    total_samples: u64,
    start_time: Option<Instant>,
    max_peak: i16,
    sum_squared: f64,
    /// Number of sessions already recorded, used to keep `--wav` takes distinct.
    session_count: u32,
}

impl AutoWavRecorder {
    pub fn new(record_dir: PathBuf, fixed_path: Option<PathBuf>) -> Self {
        Self {
            record_dir,
            fixed_path,
            current_writer: None,
            current_file_path: None,
            total_samples: 0,
            start_time: None,
            max_peak: 0,
            sum_squared: 0.0,
            session_count: 0,
        }
    }

    fn open_writer(&mut self) -> anyhow::Result<()> {
        let path = match self.fixed_path {
            // A fixed --wav path is reused for every voice session, so the first
            // session's file would be truncated by the second. Suffix all but the
            // first session to keep each take.
            Some(ref p) => {
                let path = if self.session_count == 0 {
                    p.clone()
                } else {
                    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned());
                    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
                    let name = match (stem, ext) {
                        (Some(stem), Some(ext)) => {
                            format!("{stem}_{:03}.{ext}", self.session_count)
                        }
                        (Some(stem), None) => format!("{stem}_{:03}", self.session_count),
                        _ => format!("record_{:03}.wav", self.session_count),
                    };
                    p.with_file_name(name)
                };
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        fs::create_dir_all(parent)?;
                    }
                }
                path
            }
            None => {
                if !self.record_dir.exists() {
                    fs::create_dir_all(&self.record_dir)?;
                }
                let timestamp = Local::now().format("%Y%m%d_%H%M%S%.3f");
                self.record_dir.join(format!("record_{}.wav", timestamp))
            }
        };

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let writer = hound::WavWriter::create(&path, spec)
            .map_err(|e| anyhow::anyhow!("Failed to create WAV file at {}: {e}", path.display()))?;

        log::info!(
            "[AUDIO RECORD] Started recording -> {}",
            path.display()
        );

        self.current_writer = Some(writer);
        self.current_file_path = Some(path);
        self.session_count = self.session_count.saturating_add(1);
        self.total_samples = 0;
        self.start_time = Some(Instant::now());
        self.max_peak = 0;
        self.sum_squared = 0.0;
        Ok(())
    }

    fn finalize_writer(&mut self) {
        if let Some(writer) = self.current_writer.take() {
            let duration_sec = self.total_samples as f64 / 16_000.0;
            let path_str = self
                .current_file_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "unknown".into());

            match writer.finalize() {
                Ok(()) => {
                    let rms = if self.total_samples > 0 {
                        (self.sum_squared / self.total_samples as f64).sqrt()
                    } else {
                        0.0
                    };
                    let rms_dbfs = if rms > 1e-5 {
                        20.0 * rms.log10()
                    } else {
                        -96.0
                    };
                    let peak_norm = self.max_peak as f64 / 32768.0;
                    let peak_dbfs = if peak_norm > 1e-5 {
                        20.0 * peak_norm.log10()
                    } else {
                        -96.0
                    };

                    log::info!(
                        "[AUDIO RECORD] Saved: {} | Duration: {:.2}s ({} samples) | Peak: {:.1} dBFS | RMS: {:.1} dBFS",
                        path_str,
                        duration_sec,
                        self.total_samples,
                        peak_dbfs,
                        rms_dbfs
                    );
                }
                Err(e) => {
                    log::error!("[AUDIO RECORD] Failed to finalize WAV file {}: {e}", path_str);
                }
            }
        }
        self.current_file_path = None;
        self.start_time = None;
    }
}

impl AudioSink for AutoWavRecorder {
    fn on_stream_start(&mut self) {
        if self.current_writer.is_none() {
            if let Err(e) = self.open_writer() {
                log::error!("[AUDIO RECORD] Failed to open WAV writer: {e}");
            }
        }
    }

    fn push(&mut self, pcm: &[i16]) {
        if pcm.is_empty() {
            return;
        }
        if self.current_writer.is_none() {
            let _ = self.open_writer();
        }

        if let Some(writer) = self.current_writer.as_mut() {
            for &sample in pcm {
                if let Err(e) = writer.write_sample(sample) {
                    log::error!("[AUDIO RECORD] Write error: {e}");
                    self.current_writer = None;
                    return;
                }
                let abs_val = sample.abs();
                if abs_val > self.max_peak {
                    self.max_peak = abs_val;
                }
                let norm = sample as f64 / 32768.0;
                self.sum_squared += norm * norm;
            }
            self.total_samples += pcm.len() as u64;

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
        self.finalize_writer();
    }
}

impl Drop for AutoWavRecorder {
    fn drop(&mut self) {
        self.finalize_writer();
    }
}
