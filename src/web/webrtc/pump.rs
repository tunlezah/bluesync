//! Opus -> RTP pump (AUD-038/040/041/047). Reads the PCM bus, encodes Opus on a
//! blocking thread, and writes to an `RtpSink`. No pacer: the broadcast `recv`
//! (driven by parec's blocking read) is the clock.
use crate::audio::opus_encoder::OpusEncoder;
use crate::sys::supervisor::wait_for_shutdown;
use crate::web::webrtc::rtp::RtpTimeline;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// Sink for encoded Opus payloads tagged with their RTP timestamp + sequence.
/// The real impl (W-3c) builds an `rtp::Packet` (PT 111) and writes it to the
/// webrtc track; the test impl records the calls.
pub trait RtpSink: Send + Sync {
    fn send(
        &self,
        opus: &[u8],
        timestamp: u32,
        sequence: u16,
    ) -> impl Future<Output = Result<(), String>> + Send;
}

/// Pump PCM frames from `rx` to `sink` as encoded Opus + RTP timing, until
/// shutdown, the bus closes, or the sink errors. On `Lagged(n)` the timeline
/// skips `n` frames (AUD-040). Encoding is offloaded to `spawn_blocking` (AUD-041).
pub async fn run_opus_pump<S: RtpSink>(
    mut rx: broadcast::Receiver<Vec<f32>>,
    encoder: Arc<Mutex<OpusEncoder>>,
    sink: &S,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut timeline = RtpTimeline::new();
    loop {
        let recv = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            r = rx.recv() => r,
        };
        match recv {
            Ok(frame) => {
                let enc = encoder.clone();
                // Encode off the async runtime (AUD-041). A JoinError means the
                // encode task panicked (e.g. a poisoned encoder mutex) — end the
                // pump rather than loop forever silently; the supervisor restarts
                // it (with a fresh encoder at the factory level).
                let opus = match tokio::task::spawn_blocking(move || {
                    enc.lock().unwrap().encode_frame(&frame)
                })
                .await
                {
                    Ok(opt) => opt,
                    Err(_) => return,
                };
                if let Some(opus) = opus {
                    let (timestamp, sequence) = timeline.next_packet();
                    if sink.send(&opus, timestamp, sequence).await.is_err() {
                        return;
                    }
                }
            }
            // Cap the cast so a pathological lag can't truncate (n is u64).
            Err(broadcast::error::RecvError::Lagged(n)) => {
                timeline.skip(n.min(u32::MAX as u64) as u32)
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::SAMPLES_PER_FRAME;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::{broadcast, watch};

    /// Records every (opus_len, timestamp, sequence) sent.
    #[derive(Default)]
    struct FakeRtpSink {
        sent: StdMutex<Vec<(usize, u32, u16)>>,
    }
    impl FakeRtpSink {
        fn sent(&self) -> Vec<(usize, u32, u16)> {
            self.sent.lock().unwrap().clone()
        }
    }
    impl RtpSink for FakeRtpSink {
        async fn send(&self, opus: &[u8], timestamp: u32, sequence: u16) -> Result<(), String> {
            self.sent
                .lock()
                .unwrap()
                .push((opus.len(), timestamp, sequence));
            Ok(())
        }
    }

    #[tokio::test]
    async fn pump_encodes_each_frame_with_advancing_rtp() {
        let (tx, rx) = broadcast::channel(16);
        let encoder = Arc::new(Mutex::new(OpusEncoder::new().unwrap()));
        let sink = FakeRtpSink::default();
        let (_sd_tx, sd_rx) = watch::channel(false);

        let frame = vec![0.0f32; SAMPLES_PER_FRAME];
        let driver = async {
            for _ in 0..3 {
                tx.send(frame.clone()).unwrap();
            }
            drop(tx); // close the bus -> pump drains the 3 then sees Closed -> returns
        };
        tokio::join!(run_opus_pump(rx, encoder, &sink, sd_rx), driver);

        let sent = sink.sent();
        assert_eq!(sent.len(), 3);
        // RTP timestamps advance by 960/packet, sequence by 1.
        assert_eq!((sent[0].1, sent[0].2), (0, 0));
        assert_eq!((sent[1].1, sent[1].2), (960, 1));
        assert_eq!((sent[2].1, sent[2].2), (1920, 2));
        // Each produced a non-empty Opus payload.
        assert!(sent.iter().all(|(len, _, _)| *len > 0));
    }

    #[tokio::test]
    async fn pump_stops_on_shutdown() {
        let (tx, rx) = broadcast::channel(16);
        let encoder = Arc::new(Mutex::new(OpusEncoder::new().unwrap()));
        let sink = FakeRtpSink::default();
        let (sd_tx, sd_rx) = watch::channel(true); // already shut down
        let _tx = tx; // keep the bus open so only shutdown ends the loop
        run_opus_pump(rx, encoder, &sink, sd_rx).await;
        let _ = sd_tx; // (silence unused)
        assert!(sink.sent().is_empty());
    }
}
