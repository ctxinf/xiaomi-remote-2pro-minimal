//! Audio DSP: Frame Accumulator, Smoothing, Gain & Audio Level Metrics.

/// Accumulates variable-length BLE notification chunks into fixed-size ADPCM frames.
#[derive(Debug, Default, Clone)]
pub struct FrameAccumulator {
    pending: Vec<u8>,
}

impl FrameAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends incoming data chunk and calls `emit` for each accumulated full frame.
    pub fn push(&mut self, data: &[u8], frame_size: usize, mut emit: impl FnMut(&[u8])) {
        if frame_size == 0 {
            return;
        }
        self.pending.extend_from_slice(data);
        let complete = self.pending.len() / frame_size;
        for i in 0..complete {
            emit(&self.pending[i * frame_size..(i + 1) * frame_size]);
        }
        self.pending.drain(..complete * frame_size);
    }

    /// Takes any buffered bytes that never formed a complete frame.
    ///
    /// The remote stops mid-frame when the mic button is released, so the tail of
    /// every recording sits here; decoding it recovers up to one frame of audio.
    pub fn take_partial(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// Applies 3-tap smoothing `(1, 2, 1) / 4` and dB gain to PCM samples.
pub fn postprocess(samples: &mut [i16], gain_db: f64) {
    if samples.len() >= 3 {
        let orig: Vec<i32> = samples.iter().map(|&s| s as i32).collect();
        for i in 1..orig.len() - 1 {
            samples[i] = ((orig[i - 1] + 2 * orig[i] + orig[i + 1]) >> 2) as i16;
        }
    }
    let gain_db = if gain_db.is_finite() {
        gain_db.clamp(-24.0, 24.0)
    } else {
        0.0
    };
    if gain_db != 0.0 {
        let gain = 10f64.powf(gain_db / 20.0);
        for s in samples.iter_mut() {
            *s = ((*s as f64) * gain).round().clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}

/// Calculates audio metrics (RMS dBFS, Peak Amplitude, Peak dBFS) for a block of PCM samples.
#[derive(Debug, Clone, Copy)]
pub struct AudioMetrics {
    pub peak_sample: i16,
    pub peak_dbfs: f64,
    pub rms_dbfs: f64,
}

pub fn calculate_metrics(samples: &[i16]) -> AudioMetrics {
    if samples.is_empty() {
        return AudioMetrics {
            peak_sample: 0,
            peak_dbfs: -96.0,
            rms_dbfs: -96.0,
        };
    }
    let mut sum_sq = 0.0f64;
    let mut peak = 0i16;
    for &s in samples {
        let abs_val = s.abs();
        if abs_val > peak {
            peak = abs_val;
        }
        let norm = s as f64 / 32768.0;
        sum_sq += norm * norm;
    }
    let rms = (sum_sq / samples.len() as f64).sqrt();
    let rms_dbfs = if rms > 1e-5 {
        20.0 * rms.log10()
    } else {
        -96.0
    };
    let peak_norm = peak as f64 / 32768.0;
    let peak_dbfs = if peak_norm > 1e-5 {
        20.0 * peak_norm.log10()
    } else {
        -96.0
    };

    AudioMetrics {
        peak_sample: peak,
        peak_dbfs,
        rms_dbfs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator() {
        let mut acc = FrameAccumulator::new();
        let mut frames = Vec::new();
        acc.push(&[1, 2, 3], 4, |f| frames.push(f.to_vec()));
        assert!(frames.is_empty());
        acc.push(&[4, 5, 6, 7, 8], 4, |f| frames.push(f.to_vec()));
        assert_eq!(frames, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }

    #[test]
    fn test_postprocess_smoothing_and_gain() {
        let mut samples = vec![100i16, 200, 100];
        postprocess(&mut samples, 0.0);
        assert_eq!(samples, vec![100, 150, 100]);
    }

    #[test]
    fn test_metrics() {
        let samples = vec![0i16, 16384, -16384, 0];
        let m = calculate_metrics(&samples);
        assert_eq!(m.peak_sample, 16384);
        assert!((m.peak_dbfs - (-6.02)).abs() < 0.1);
    }
}
