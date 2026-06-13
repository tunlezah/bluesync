//! CASTV2 TLS client — URL helpers, no-verify TLS config, and the
//! `CastSession` that connects to a Chromecast and casts an HTTP stream URL.
//!
//! # URL helpers (tested)
//! - [`server_lan_ip`] — extract the first usable LAN IPv4 from `hostname -I` output.
//! - [`stream_url`] — build the `/api/stream/audio.aac` URL.
//!
//! # TLS (no-verify)
//! Chromecasts use self-signed TLS certificates on a LAN.  A no-verify
//! `rustls::ClientConfig` is acceptable and explicitly documented here.
//! [`insecure_tls_config`] builds one via the ring cryptography provider.
//!
//! # Cast session (glue — not unit-tested)
//! [`start_cast`] spawns a tokio task that drives the full CASTV2 flow:
//! TLS-connect → CONNECT(receiver-0) → LAUNCH(CC1AD845) → RECEIVER_STATUS →
//! CONNECT(transportId) → LOAD(stream url) → PING/PONG keepalive.
//! The caller receives a [`CastHandle`]; calling [`CastHandle::stop`] shuts
//! the session down gracefully.

use crate::cast::messages::{
    self, DEFAULT_MEDIA_RECEIVER_APP_ID, NS_CONNECTION, NS_HEARTBEAT, NS_MEDIA, NS_RECEIVER,
};
use crate::cast::proto::{decode_frame, encode_frame, string_message};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

// ─── URL helpers ─────────────────────────────────────────────────────────────

/// Extract the first usable LAN IPv4 from the output of `hostname -I`.
///
/// `hostname -I` prints space-separated addresses (IPv4 and IPv6).  This
/// function returns the first IPv4 that is:
/// - not loopback (`127.x.x.x`)
/// - not link-local (`169.254.x.x`)
///
/// Within the non-excluded addresses it prefers a private-range address
/// (`192.168./10./172.16–31.`) before falling back to the first IPv4 found.
///
/// Returns `None` if no usable IPv4 is present.
pub fn server_lan_ip(hostname_i_output: &str) -> Option<String> {
    let mut first_ipv4: Option<String> = None;

    for token in hostname_i_output.split_whitespace() {
        // Must look like an IPv4: four decimal octets separated by dots.
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 4 || parts.iter().any(|p| p.parse::<u8>().is_err()) {
            // Not an IPv4 — skip (could be IPv6 or junk).
            continue;
        }

        // Skip loopback and link-local.
        if token.starts_with("127.") || token.starts_with("169.254.") {
            continue;
        }

        // Remember the very first non-excluded IPv4 (fallback).
        if first_ipv4.is_none() {
            first_ipv4 = Some(token.to_string());
        }

        // Prefer a private-range address — return immediately.
        if token.starts_with("192.168.") || token.starts_with("10.") || is_rfc1918_172(token) {
            return Some(token.to_string());
        }
    }

    first_ipv4
}

/// Return `true` for `172.16.0.0/12` (172.16.x.x – 172.31.x.x).
fn is_rfc1918_172(addr: &str) -> bool {
    let parts: Vec<&str> = addr.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    if parts[0] != "172" {
        return false;
    }
    parts[1]
        .parse::<u8>()
        .map(|b| (16..=31).contains(&b))
        .unwrap_or(false)
}

/// Build the HTTP stream URL that the Chromecast should pull.
///
/// Format: `http://{lan_ip}:{port}/api/stream/audio.aac`
pub fn stream_url(lan_ip: &str, port: u16) -> String {
    format!("http://{lan_ip}:{port}/api/stream/audio.aac")
}

// ─── No-verify TLS config ─────────────────────────────────────────────────────

/// Build a `rustls::ClientConfig` that accepts ANY server certificate.
///
/// Chromecasts present self-signed TLS certificates on port 8009.  On a
/// trusted LAN this is acceptable — the Chromecast protocol provides no
/// mechanism for certificate pinning or CA validation.  The no-verify
/// approach is documented explicitly here for auditability.
pub fn insecure_tls_config() -> Arc<rustls::ClientConfig> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme};

    #[derive(Debug)]
    struct NoCertVerify(Vec<SignatureScheme>);

    impl ServerCertVerifier for NoCertVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.clone()
        }
    }

    let schemes = rustls::crypto::ring::default_provider()
        .signature_verification_algorithms
        .supported_schemes();

    let cfg = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCertVerify(schemes)))
        .with_no_client_auth();

    Arc::new(cfg)
}

// ─── Cast session ─────────────────────────────────────────────────────────────

/// Handle to a running CASTV2 session.
///
/// Drop or call [`CastHandle::stop`] to shut the session down.
pub struct CastHandle {
    shutdown: watch::Sender<bool>,
}

impl CastHandle {
    /// Signal the session task to shut down gracefully.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
    }
}

impl Drop for CastHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn a CASTV2 session that connects to `ip:port` and casts `media_url`.
///
/// The session is best-effort: any connection or protocol error is logged and
/// the task exits quietly.  The caller can detect this via the returned
/// [`CastHandle`] going stale (the watch channel will close when the task
/// exits) but for the current use-case the controller simply stops the
/// session on teardown.
pub fn start_cast(ip: String, port: u16, media_url: String) -> CastHandle {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(run_cast_session(ip, port, media_url, shutdown_rx));
    CastHandle {
        shutdown: shutdown_tx,
    }
}

/// The CASTV2 session task.
///
/// Flow:
/// 1. TCP + TLS connect to `ip:port` using the insecure config.
/// 2. Send CONNECT(receiver-0) + LAUNCH(CC1AD845).
/// 3. Read frames in a `tokio::select!` loop:
///    - RECEIVER_STATUS with CC1AD845 transportId → CONNECT(transportId) + LOAD.
///    - PING → PONG.
///    - other → ignore.
/// 4. Heartbeat PING sent every ~5 s.
/// 5. On shutdown signal → send CLOSE(receiver-0) best-effort, then return.
async fn run_cast_session(
    ip: String,
    port: u16,
    media_url: String,
    mut shutdown: watch::Receiver<bool>,
) {
    // ── 1. TCP + TLS connect ───────────────────────────────────────────────
    let tcp = match TcpStream::connect(format!("{ip}:{port}")).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soundsync/cast: TCP connect to {ip}:{port} failed: {e}");
            return;
        }
    };

    let tls_cfg = insecure_tls_config();
    let connector = TlsConnector::from(tls_cfg);

    // The Chromecast presents a self-signed cert with no matching DNS name.
    // We use IpAddress — with no-verify the ServerName is irrelevant but
    // IpAddress is more semantically correct than a dummy DnsName.
    let server_name = match ip.parse::<std::net::IpAddr>() {
        Ok(addr) => rustls::pki_types::ServerName::IpAddress(addr.into()),
        Err(e) => {
            eprintln!("soundsync/cast: cannot parse {ip:?} as IP address: {e}");
            return;
        }
    };

    let tls_stream = match connector.connect(server_name, tcp).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("soundsync/cast: TLS handshake with {ip}:{port} failed: {e}");
            return;
        }
    };

    let (mut read_half, mut write_half) = tokio::io::split(tls_stream);
    eprintln!("soundsync/cast: connected to {ip}:{port}");

    // ── 2. CONNECT(receiver-0) + LAUNCH(CC1AD845) ────────────────────────
    let connect_msg = string_message(NS_CONNECTION, "receiver-0", &messages::connect());
    if let Err(e) = write_half.write_all(&encode_frame(&connect_msg)).await {
        eprintln!("soundsync/cast: send CONNECT failed: {e}");
        return;
    }

    let launch_msg = string_message(
        NS_RECEIVER,
        "receiver-0",
        &messages::launch(1, DEFAULT_MEDIA_RECEIVER_APP_ID),
    );
    if let Err(e) = write_half.write_all(&encode_frame(&launch_msg)).await {
        eprintln!("soundsync/cast: send LAUNCH failed: {e}");
        return;
    }
    eprintln!("soundsync/cast: sent LAUNCH (default media receiver)");

    // ── 3. Read/write loop ────────────────────────────────────────────────
    let mut read_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    let mut loaded = false;
    let mut req_id: u32 = 2;

    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(5));
    // Skip the immediate first tick so we don't send a PING before the handshake settles.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            biased;

            // Shutdown signal.
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    eprintln!("soundsync/cast: stopping cast session");
                    // Best-effort CLOSE.
                    let close_msg = string_message(NS_CONNECTION, "receiver-0", &messages::close());
                    let _ = write_half.write_all(&encode_frame(&close_msg)).await;
                    return;
                }
            }

            // Incoming bytes from the Chromecast.
            n = read_half.read(&mut tmp) => {
                match n {
                    Err(e) => {
                        eprintln!("soundsync/cast: read error: {e}");
                        return;
                    }
                    Ok(0) => {
                        // EOF — Chromecast closed the connection.
                        return;
                    }
                    Ok(n) => {
                        read_buf.extend_from_slice(&tmp[..n]);

                        // Drain all complete frames from read_buf.
                        loop {
                            match decode_frame(&read_buf) {
                                None => break,
                                Some((msg, consumed)) => {
                                    read_buf.drain(..consumed);
                                    let payload = msg.payload_utf8.as_deref().unwrap_or("");
                                    match messages::message_type(payload).as_deref() {
                                        Some("PING") => {
                                            // Reply with PONG on the heartbeat namespace.
                                            let pong = string_message(NS_HEARTBEAT, &msg.source_id, &messages::pong());
                                            if let Err(e) = write_half.write_all(&encode_frame(&pong)).await {
                                                eprintln!("soundsync/cast: send PONG failed: {e}");
                                                return;
                                            }
                                        }
                                        Some("RECEIVER_STATUS") if !loaded => {
                                            // Check if CC1AD845 has been launched.
                                            if let Some(tid) = messages::parse_launched_transport_id(payload, DEFAULT_MEDIA_RECEIVER_APP_ID) {
                                                eprintln!("soundsync/cast: media session {tid} ready");
                                                // CONNECT to the transport channel.
                                                let conn2 = string_message(NS_CONNECTION, &tid, &messages::connect());
                                                if let Err(e) = write_half.write_all(&encode_frame(&conn2)).await {
                                                    eprintln!("soundsync/cast: send CONNECT(transport) failed: {e}");
                                                    return;
                                                }
                                                // LOAD the stream.
                                                let load_msg = string_message(
                                                    NS_MEDIA,
                                                    &tid,
                                                    &messages::load(req_id, &media_url, "audio/aac"),
                                                );
                                                req_id += 1;
                                                if let Err(e) = write_half.write_all(&encode_frame(&load_msg)).await {
                                                    eprintln!("soundsync/cast: send LOAD failed: {e}");
                                                    return;
                                                }
                                                eprintln!("soundsync/cast: sent LOAD {media_url}");
                                                loaded = true;
                                            }
                                        }
                                        Some("MEDIA_STATUS") => {
                                            eprintln!("soundsync/cast: media status received (playing)");
                                        }
                                        _ => {} // ignore other messages
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Periodic heartbeat PING.
            _ = heartbeat.tick() => {
                let ping = string_message(NS_HEARTBEAT, "receiver-0", &messages::ping());
                if let Err(e) = write_half.write_all(&encode_frame(&ping)).await {
                    eprintln!("soundsync/cast: send PING failed: {e}");
                    return;
                }
            }
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── server_lan_ip ─────────────────────────────────────────────────────

    #[test]
    fn server_lan_ip_prefers_private_ipv4() {
        // Typical multi-address output: IPv6 first, then a private IPv4.
        let output = "fe80::1 192.168.1.105 127.0.0.1";
        assert_eq!(server_lan_ip(output), Some("192.168.1.105".to_string()));
    }

    #[test]
    fn server_lan_ip_picks_first_private_when_multiple() {
        // Two private IPv4s: should return the first one.
        let output = "192.168.1.10 192.168.1.20";
        assert_eq!(server_lan_ip(output), Some("192.168.1.10".to_string()));
    }

    #[test]
    fn server_lan_ip_accepts_10_dot_range() {
        let output = "fe80::1 10.0.0.55";
        assert_eq!(server_lan_ip(output), Some("10.0.0.55".to_string()));
    }

    #[test]
    fn server_lan_ip_accepts_172_16_range() {
        let output = "172.20.0.3";
        assert_eq!(server_lan_ip(output), Some("172.20.0.3".to_string()));
    }

    #[test]
    fn server_lan_ip_rejects_172_15() {
        // 172.15.x.x is NOT in RFC-1918 (must be 16–31); falls back to first non-excluded.
        let output = "172.15.0.1";
        // 172.15 is a valid IPv4, not loopback/link-local, so it's returned as fallback.
        assert_eq!(server_lan_ip(output), Some("172.15.0.1".to_string()));
    }

    #[test]
    fn server_lan_ip_skips_loopback() {
        let output = "127.0.0.1";
        assert_eq!(server_lan_ip(output), None);
    }

    #[test]
    fn server_lan_ip_skips_link_local() {
        let output = "169.254.1.1";
        assert_eq!(server_lan_ip(output), None);
    }

    #[test]
    fn server_lan_ip_only_loopback_returns_none() {
        let output = "127.0.0.1 127.0.1.1";
        assert_eq!(server_lan_ip(output), None);
    }

    #[test]
    fn server_lan_ip_empty_returns_none() {
        assert_eq!(server_lan_ip(""), None);
    }

    #[test]
    fn server_lan_ip_only_ipv6_returns_none() {
        let output = "fe80::1 ::1 2001:db8::1";
        assert_eq!(server_lan_ip(output), None);
    }

    #[test]
    fn server_lan_ip_fallback_to_non_private_when_no_private() {
        // A public IPv4 (not loopback, not link-local, not private) should be
        // returned as a fallback when no private range address is found.
        let output = "8.8.8.8";
        assert_eq!(server_lan_ip(output), Some("8.8.8.8".to_string()));
    }

    #[test]
    fn server_lan_ip_prefers_private_over_public() {
        // Public comes first, but private should be preferred.
        let output = "8.8.8.8 192.168.0.1";
        assert_eq!(server_lan_ip(output), Some("192.168.0.1".to_string()));
    }

    #[test]
    fn server_lan_ip_realistic_hostname_i_output() {
        // Real-world: IPv4 first, then IPv6 link-local.
        let output = "192.168.1.47 fe80::a00:27ff:fe4e:66a1";
        assert_eq!(server_lan_ip(output), Some("192.168.1.47".to_string()));
    }

    // ── stream_url ────────────────────────────────────────────────────────

    #[test]
    fn stream_url_format() {
        assert_eq!(
            stream_url("192.168.1.47", 8080),
            "http://192.168.1.47:8080/api/stream/audio.aac"
        );
    }

    #[test]
    fn stream_url_different_port() {
        assert_eq!(
            stream_url("10.0.0.1", 9090),
            "http://10.0.0.1:9090/api/stream/audio.aac"
        );
    }

    #[test]
    fn stream_url_port_80() {
        assert_eq!(
            stream_url("192.168.0.5", 80),
            "http://192.168.0.5:80/api/stream/audio.aac"
        );
    }
}
