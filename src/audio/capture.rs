//! Capture unit: PCM broadcast bus + a frame pump that turns a raw f32 byte
//! stream into 20ms frames on the bus (AUD-036/039). `run_capture` glues the
//! process seam to the pump for use as a supervised unit.
use crate::audio::frame::{BROADCAST_CAPACITY, BYTES_PER_FRAME};
use crate::audio::pw::capture_cmd::CaptureCommand;
use crate::sys::process::ProcessSpawner;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::broadcast;

/// Create the PCM broadcast bus. Consumers call `.subscribe()` on the returned
/// sender. Capacity is `BROADCAST_CAPACITY` (~5.12s) — see AUD-039.
pub fn new_pcm_bus() -> broadcast::Sender<Vec<f32>> {
    broadcast::channel(BROADCAST_CAPACITY).0
}

/// Read exact 7680-byte (1920 little-endian f32) frames from `reader` and
/// broadcast each as a `Vec<f32>`. Returns `Ok(())` at a clean end of stream
/// (EOF on a frame boundary) and `Err` on a read error. A trailing partial
/// frame is dropped. Send errors (no subscribers) are ignored — audio with no
/// listeners is still a live stream.
///
/// A `Vec<f32>` is allocated per frame on purpose: `broadcast` needs an owned
/// value to clone to each subscriber, so an in-place buffer reuse is not
/// possible here (would require `Arc<[f32]>`). `send` is non-blocking, so a
/// slow or lagging subscriber never stalls the pump (it sees `Lagged` on its
/// own `recv`).
pub async fn pump_frames<R>(mut reader: R, tx: &broadcast::Sender<Vec<f32>>) -> io::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    let mut buf = vec![0u8; BYTES_PER_FRAME];
    loop {
        match reader.read_exact(&mut buf).await {
            Ok(_) => {
                let frame: Vec<f32> = buf
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
                let _ = tx.send(frame);
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// Supervised capture unit: spawn the capture tool with stdout piped, then pump
/// its PCM stdout into the bus. Returns when the stream ends (clean or error) —
/// the supervisor restarts it; the dropped child is killed via `kill_on_drop`.
pub async fn run_capture<S: ProcessSpawner>(
    spawner: &S,
    command: &CaptureCommand,
    tx: &broadcast::Sender<Vec<f32>>,
) -> io::Result<()> {
    let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
    let (_child, stdout) = spawner.spawn_capture(command.program, &args).await?;
    pump_frames(stdout, tx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::frame::{BYTES_PER_FRAME, SAMPLES_PER_FRAME};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::ReadBuf;

    // A reader that always errors, to exercise the error path.
    struct ErrReader;
    impl tokio::io::AsyncRead for ErrReader {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(std::io::Error::other("boom")))
        }
    }

    #[tokio::test]
    async fn pumps_full_frames_and_drops_trailing_partial() {
        let tx = new_pcm_bus();
        let mut rx = tx.subscribe();
        let bytes = vec![0u8; BYTES_PER_FRAME * 2 + 5]; // 2 full frames + stray bytes
        pump_frames(&bytes[..], &tx).await.unwrap();
        let f1 = rx.try_recv().unwrap();
        let f2 = rx.try_recv().unwrap();
        assert_eq!(f1.len(), SAMPLES_PER_FRAME);
        assert_eq!(f2.len(), SAMPLES_PER_FRAME);
        assert!(rx.try_recv().is_err()); // no third frame
    }

    #[tokio::test]
    async fn decodes_little_endian_f32_samples() {
        let tx = new_pcm_bus();
        let mut rx = tx.subscribe();
        let mut bytes = vec![0u8; BYTES_PER_FRAME];
        bytes[0..4].copy_from_slice(&1.0f32.to_le_bytes());
        bytes[4..8].copy_from_slice(&(-2.5f32).to_le_bytes());
        pump_frames(&bytes[..], &tx).await.unwrap();
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame[0], 1.0);
        assert_eq!(frame[1], -2.5);
        assert_eq!(frame[2], 0.0);
    }

    #[tokio::test]
    async fn empty_stream_yields_no_frames_ok() {
        let tx = new_pcm_bus();
        let mut rx = tx.subscribe();
        let empty: &[u8] = &[];
        pump_frames(empty, &tx).await.unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn read_error_propagates() {
        let tx = new_pcm_bus();
        let err = pump_frames(ErrReader, &tx).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }

    #[tokio::test]
    async fn pump_ok_when_no_subscribers() {
        // A live stream with no listeners must not error (broadcast send fails
        // with no receivers; we ignore that).
        let tx = new_pcm_bus();
        let bytes = vec![0u8; BYTES_PER_FRAME];
        pump_frames(&bytes[..], &tx).await.unwrap();
    }

    #[tokio::test]
    async fn pump_does_not_stall_when_subscriber_lags() {
        // A subscribed-but-never-read receiver overflowing the bus capacity must
        // not stall or abort the pump (AUD-039: one slow consumer can't break
        // the stream). Pump more than BROADCAST_CAPACITY frames.
        use crate::audio::frame::BROADCAST_CAPACITY;
        let tx = new_pcm_bus();
        let _rx = tx.subscribe(); // subscribed, never drained
        let bytes = vec![0u8; BYTES_PER_FRAME * (BROADCAST_CAPACITY + 5)];
        pump_frames(&bytes[..], &tx).await.unwrap();
    }

    #[tokio::test]
    async fn run_capture_spawns_tool_and_streams_frames() {
        use crate::audio::pw::capture_cmd::{build_capture_command, CaptureSource, CaptureTool};
        use crate::sys::testing::FakeProcessSpawner;

        let one_frame = vec![0u8; BYTES_PER_FRAME];
        let spawner = FakeProcessSpawner::new(None).with_stdout(one_frame);
        let cmd = build_capture_command(
            CaptureTool::Parec,
            &CaptureSource::NullSinkMonitor("soundsync-capture".into()),
            50,
        );
        let tx = new_pcm_bus();
        let mut rx = tx.subscribe();

        run_capture(&spawner, &cmd, &tx).await.unwrap();

        // One frame made it onto the bus.
        let frame = rx.try_recv().unwrap();
        assert_eq!(frame.len(), SAMPLES_PER_FRAME);
        // The capture tool was spawned with the built parec argv.
        let (prog, args) = &spawner.spawns()[0];
        assert_eq!(prog, "parec");
        assert!(args.contains(&"--device=soundsync-capture.monitor".to_string()));
    }
}
