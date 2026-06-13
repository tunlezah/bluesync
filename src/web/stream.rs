//! HTTP audio fallback: a per-client `parec | ffmpeg` pipeline streamed as a
//! chunked HTTP body (AUD-042/043/044). Each client gets a dedicated pipeline
//! (bypasses the broadcast bus — the anti-stutter design); `kill_on_drop` reaps
//! it on disconnect. parec taps `soundsync-capture.monitor` (AUD-034).

/// Encoded HTTP stream format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFormat {
    Aac,
    Mp3,
}

impl StreamFormat {
    /// HTTP `Content-Type` for this format.
    pub fn content_type(self) -> &'static str {
        match self {
            StreamFormat::Aac => "audio/aac",
            StreamFormat::Mp3 => "audio/mpeg",
        }
    }

    /// The ffmpeg codec/container args for this format.
    fn ffmpeg_codec_args(self) -> &'static str {
        match self {
            // AAC-LC 256k ADTS (AUD-043).
            StreamFormat::Aac => "-c:a aac -b:a 256k -f adts",
            // MP3 192k (AUD-044).
            StreamFormat::Mp3 => "-c:a libmp3lame -b:a 192k -f mp3",
        }
    }
}

/// Build the `parec | ffmpeg` shell pipeline for `format`, capturing `device`
/// (the null-sink monitor). s16le/48k/2ch in; low-latency flags throughout.
pub fn build_stream_command(format: StreamFormat, device: &str) -> String {
    format!(
        "parec --device={device} --format=s16le --rate=48000 --channels=2 --latency-msec=50 \
         | ffmpeg -hide_banner -loglevel quiet -fflags +nobuffer \
                  -f s16le -ar 48000 -ac 2 -i pipe:0 \
                  {codec} -flush_packets 1 pipe:1",
        codec = format.ffmpeg_codec_args()
    )
}

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::stream;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::Command;

/// The capture monitor parec reads (the null sink's monitor; AUD-034).
const CAPTURE_MONITOR: &str = "soundsync-capture.monitor";

/// axum handler: `GET /api/stream/audio.aac`.
pub async fn stream_aac() -> Response {
    stream_response(StreamFormat::Aac).await
}

/// axum handler: `GET /api/stream/audio.mp3`.
pub async fn stream_mp3() -> Response {
    stream_response(StreamFormat::Mp3).await
}

async fn stream_response(format: StreamFormat) -> Response {
    let cmd = build_stream_command(format, CAPTURE_MONITOR);
    let mut child = match Command::new("sh")
        .args(["-c", &cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    // The unfold state owns the child, so the pipeline lives as long as the
    // response body; dropping it (client disconnect) drops the child →
    // kill_on_drop tears down parec+ffmpeg.
    let body_stream = stream::unfold(
        (BufReader::new(stdout), child),
        |(mut reader, child)| async move {
            let mut buf = vec![0u8; 8192];
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => None, // EOF / error -> end the stream
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok::<_, std::io::Error>(buf), (reader, child)))
                }
            }
        },
    );

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, format.content_type())
        .header(header::CACHE_CONTROL, "no-cache, no-store")
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_command_has_expected_pipeline() {
        let cmd = build_stream_command(StreamFormat::Aac, "soundsync-capture.monitor");
        // parec side (AUD-034/037): the null-sink monitor, s16le/48k/2ch, 50ms.
        assert!(cmd.contains("parec --device=soundsync-capture.monitor"));
        assert!(cmd.contains("--format=s16le"));
        assert!(cmd.contains("--rate=48000"));
        assert!(cmd.contains("--channels=2"));
        assert!(cmd.contains("--latency-msec=50"));
        // ffmpeg side (AUD-043): AAC-LC 256k ADTS, low-latency flags.
        assert!(cmd.contains("-f s16le -ar 48000 -ac 2 -i pipe:0"));
        assert!(cmd.contains("-c:a aac"));
        assert!(cmd.contains("-b:a 256k"));
        assert!(cmd.contains("-f adts"));
        assert!(cmd.contains("-fflags +nobuffer"));
        assert!(cmd.contains("-flush_packets 1"));
        assert!(cmd.contains("pipe:1"));
    }

    #[test]
    fn mp3_command_uses_libmp3lame_192k() {
        let cmd = build_stream_command(StreamFormat::Mp3, "soundsync-capture.monitor");
        assert!(cmd.contains("-c:a libmp3lame"));
        assert!(cmd.contains("-b:a 192k"));
        assert!(cmd.contains("-f mp3"));
        assert!(!cmd.contains("-c:a aac"));
    }

    #[test]
    fn content_types() {
        assert_eq!(StreamFormat::Aac.content_type(), "audio/aac");
        assert_eq!(StreamFormat::Mp3.content_type(), "audio/mpeg");
    }
}
