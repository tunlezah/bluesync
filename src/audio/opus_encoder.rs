//! Opus encoder for WebRTC audio: 48 kHz stereo, 128 kbps, Application::Audio
//! (AUD-045). Encodes one canonical 20 ms frame (SAMPLES_PER_FRAME interleaved
//! f32) per call.

/// Wraps the `opus` crate encoder.
pub struct OpusEncoder {
    encoder: opus::Encoder,
    encode_buf: Vec<u8>,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, opus::Error> {
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)?;
        encoder.set_bitrate(opus::Bitrate::Bits(128_000))?;
        Ok(Self {
            // 4000 > max Opus frame (~1275 bytes).
            encoder,
            encode_buf: vec![0u8; 4000],
        })
    }

    /// Encode one 20 ms frame of interleaved f32 PCM (SAMPLES_PER_FRAME values).
    /// Returns the Opus packet, or `None` on encode error / zero output.
    pub fn encode_frame(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        match self.encoder.encode_float(pcm, &mut self.encode_buf) {
            Ok(len) if len > 0 => Some(self.encode_buf[..len].to_vec()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::SAMPLES_PER_FRAME;

    #[test]
    fn creates_encoder() {
        assert!(OpusEncoder::new().is_ok());
    }

    #[test]
    fn encodes_silence_to_bounded_packet() {
        let mut enc = OpusEncoder::new().unwrap();
        let silence = vec![0.0f32; SAMPLES_PER_FRAME];
        let pkt = enc
            .encode_frame(&silence)
            .expect("opus emits a packet for silence");
        assert!(!pkt.is_empty());
        assert!(pkt.len() < 4000);
    }

    #[test]
    fn encodes_a_tone() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut pcm = Vec::with_capacity(SAMPLES_PER_FRAME);
        for i in 0..(SAMPLES_PER_FRAME / 2) {
            let t = i as f32 / 48_000.0;
            let s = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.5;
            pcm.push(s); // L
            pcm.push(s); // R
        }
        assert!(enc.encode_frame(&pcm).is_some());
    }

    #[test]
    fn wrong_frame_size_returns_none() {
        let mut enc = OpusEncoder::new().unwrap();
        // 100 samples is not a valid 48k Opus frame size -> encode errors -> None.
        assert!(enc.encode_frame(&vec![0.0f32; 100]).is_none());
    }
}
