//! IMA/DVI ADPCM Decoder for ATVV 16kHz Voice Stream.
//!
//! ATVV audio packs two 4-bit ADPCM samples per byte (high nibble first).
//! The decoder decompresses them into 16-bit linear PCM at 16 kHz.

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

const INDEX_TABLE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// IMA ADPCM State Machine.
#[derive(Debug, Clone, Default)]
pub struct ImaAdpcmDecoder {
    predictor: i32,
    step_index: i32,
}

impl ImaAdpcmDecoder {
    pub fn new() -> Self {
        Self {
            predictor: 0,
            step_index: 0,
        }
    }

    /// Reset internal state (used upon start or AUDIO_SYNC).
    pub fn reset(&mut self, predictor: i32, step_index: i32) {
        self.predictor = predictor.clamp(-32_768, 32_767);
        self.step_index = step_index.clamp(0, 88);
    }

    /// Decode raw ADPCM bytes into output PCM buffer.
    pub fn decode_into(&mut self, data: &[u8], out: &mut Vec<i16>) {
        out.reserve(data.len() * 2);
        for byte in data {
            out.push(self.decode_nibble((byte >> 4) as i32));
            out.push(self.decode_nibble((byte & 0x0F) as i32));
        }
    }

    fn decode_nibble(&mut self, n: i32) -> i16 {
        let step = STEP_TABLE[self.step_index as usize];
        let mut diff = step >> 3;
        if n & 1 != 0 {
            diff += step >> 2;
        }
        if n & 2 != 0 {
            diff += step >> 1;
        }
        if n & 4 != 0 {
            diff += step;
        }
        self.predictor += if n & 8 != 0 { -diff } else { diff };
        self.predictor = self.predictor.clamp(-32_768, 32_767);
        self.step_index = (self.step_index + INDEX_TABLE[(n & 7) as usize]).clamp(0, 88);
        self.predictor as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adpcm_decoding() {
        let mut dec = ImaAdpcmDecoder::new();
        let mut out = Vec::new();
        dec.decode_into(&[0x77], &mut out);
        assert_eq!(out, vec![11, 41]);
    }

    #[test]
    fn test_adpcm_reset() {
        let mut dec = ImaAdpcmDecoder::new();
        dec.reset(100, 5);
        let mut out = Vec::new();
        dec.decode_into(&[0x00], &mut out);
        assert_eq!(out, vec![101, 102]);
    }
}
