//! RTP timestamp/sequence bookkeeping for the Opus track (AUD-040/047).

/// RTP timestamp units per 20 ms Opus packet (48 kHz clock × 0.02 s = 960).
pub const RTP_SAMPLES_PER_FRAME: u32 = 960;

/// Tracks the RTP timestamp + sequence number for one outgoing Opus stream.
#[derive(Debug, Default)]
pub struct RtpTimeline {
    timestamp: u32,
    sequence: u16,
}

impl RtpTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Values for the packet about to be sent, then advance for the next one
    /// (ts += 960, seq += 1, both wrapping).
    pub fn next_packet(&mut self) -> (u32, u16) {
        let now = (self.timestamp, self.sequence);
        self.timestamp = self.timestamp.wrapping_add(RTP_SAMPLES_PER_FRAME);
        self.sequence = self.sequence.wrapping_add(1);
        now
    }

    /// After the capture broadcast dropped `n` frames (`Lagged(n)`), skip the
    /// timeline ahead so it stays aligned to wall-clock (AUD-040): ts += n*960,
    /// seq += n. Without this the browser jitter buffer "catches up" by playing
    /// fast → stutter.
    pub fn skip(&mut self, n: u32) {
        self.timestamp = self
            .timestamp
            .wrapping_add(RTP_SAMPLES_PER_FRAME.wrapping_mul(n));
        self.sequence = self.sequence.wrapping_add(n as u16);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_per_packet() {
        let mut t = RtpTimeline::new();
        assert_eq!(t.next_packet(), (0, 0));
        assert_eq!(t.next_packet(), (RTP_SAMPLES_PER_FRAME, 1));
        assert_eq!(t.next_packet(), (RTP_SAMPLES_PER_FRAME * 2, 2));
    }

    #[test]
    fn skip_advances_by_n_frames() {
        let mut t = RtpTimeline::new();
        assert_eq!(t.next_packet(), (0, 0)); // ts now 960, seq 1
        t.skip(3); // ts += 3*960, seq += 3  -> ts 3840, seq 4
        assert_eq!(t.next_packet(), (RTP_SAMPLES_PER_FRAME * 4, 4));
    }

    #[test]
    fn sequence_wraps_u16() {
        let mut t = RtpTimeline::new();
        t.skip(u16::MAX as u32); // seq -> 65535
        let (_, seq) = t.next_packet();
        assert_eq!(seq, u16::MAX);
        let (_, seq) = t.next_packet();
        assert_eq!(seq, 0); // wrapped
    }
}
