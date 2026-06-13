//! Canonical PCM frame format (AUD-036). 48kHz/2ch/f32, 20ms frames.

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// Samples per channel per frame (20ms @ 48kHz).
pub const FRAME_SIZE: usize = 960;
/// Interleaved f32 values per frame.
pub const SAMPLES_PER_FRAME: usize = FRAME_SIZE * CHANNELS as usize; // 1920
/// Bytes per frame (f32 = 4 bytes).
pub const BYTES_PER_FRAME: usize = SAMPLES_PER_FRAME * 4; // 7680
/// PCM broadcast channel capacity (~5.12s). 64 was too small (see AUD-039).
pub const BROADCAST_CAPACITY: usize = 256;

/// Wall-clock duration of one frame, in seconds.
pub fn frame_duration_secs() -> f64 {
    FRAME_SIZE as f64 / SAMPLE_RATE as f64
}

/// Split a raw little-endian f32 byte buffer into full frames, dropping any
/// trailing partial frame. Returns one Vec<f32> of SAMPLES_PER_FRAME per frame.
pub fn split_frames(bytes: &[u8]) -> Vec<Vec<f32>> {
    bytes
        .chunks_exact(BYTES_PER_FRAME)
        .map(|chunk| {
            chunk
                .chunks_exact(4)
                // chunks_exact(4) guarantees 4 bytes, so try_into never fails.
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_legacy_invariant() {
        assert_eq!(SAMPLE_RATE, 48_000);
        assert_eq!(CHANNELS, 2);
        assert_eq!(FRAME_SIZE, 960);
        assert_eq!(SAMPLES_PER_FRAME, 1920);
        assert_eq!(BYTES_PER_FRAME, 7680);
        // Non-derived constant: this assertion IS the spec, not a formula check.
        assert_eq!(BROADCAST_CAPACITY, 256);
    }

    #[test]
    fn frame_duration_is_20ms() {
        assert!((frame_duration_secs() - 0.020).abs() < 1e-9);
    }

    #[test]
    fn split_frames_yields_full_frames_only() {
        let bytes = vec![0u8; BYTES_PER_FRAME * 2 + 5];
        let frames = split_frames(&bytes);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() == SAMPLES_PER_FRAME));
    }
}
