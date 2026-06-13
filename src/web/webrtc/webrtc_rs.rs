//! Real webrtc-rs controller + RTP sink (AUD-047/052). Integration glue: this
//! wires the version-sensitive webrtc 0.12 / rtp 0.12 async API and is NOT
//! unit-tested (real ICE/DTLS). The browser is the offerer (recvonly); the
//! server answers with a sendonly Opus `TrackLocalStaticRTP` (PT 111, 48000,
//! 2ch), one `RTCPeerConnection` per WS session (AUD-053). ICE candidates +
//! the answer travel the event bus as `SystemEvent::Webrtc*`.
use crate::audio::opus_encoder::OpusEncoder;
use crate::state::{AppStateHandle, SystemEvent};
use crate::web::webrtc::control::WebRtcController;
use crate::web::webrtc::pump::{run_opus_pump, RtpSink};
use crate::web::ws::IceCandidate;
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{broadcast, watch, Mutex};
use uuid::Uuid;

use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_OPUS};
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

/// RTP payload type for the Opus track (AUD-047). The browser offer negotiates
/// 111 for Opus; we tag every packet with it.
const OPUS_PAYLOAD_TYPE: u8 = 111;
/// STUN server for ICE (matches the legacy stack).
const STUN_URL: &str = "stun:stun.l.google.com:19302";

/// Install the rustls *ring* `CryptoProvider` as the process default. webrtc-rs
/// DTLS needs a default provider installed; idempotent (a second install is a
/// no-op). Call once at startup (W-3d / main wires it).
pub fn install_crypto_provider() {
    // `install_default` errors only if one is already installed — ignore that.
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// `RtpSink` writing encoded Opus payloads to a webrtc sendonly track as RTP
/// packets (PT 111). Holds the track + a fixed SSRC for the stream.
pub struct WebrtcRtpSink {
    track: Arc<TrackLocalStaticRTP>,
    ssrc: u32,
}

impl WebrtcRtpSink {
    pub fn new(track: Arc<TrackLocalStaticRTP>, ssrc: u32) -> Self {
        Self { track, ssrc }
    }
}

impl RtpSink for WebrtcRtpSink {
    async fn send(&self, opus: &[u8], timestamp: u32, sequence: u16) -> Result<(), String> {
        let packet = rtp::packet::Packet {
            header: rtp::header::Header {
                version: 2,
                payload_type: OPUS_PAYLOAD_TYPE,
                sequence_number: sequence,
                timestamp,
                ssrc: self.ssrc,
                ..Default::default()
            },
            payload: bytes::Bytes::copy_from_slice(opus),
        };
        self.track
            .write_rtp(&packet)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// Per-session resources kept for cleanup: the PC and the pump shutdown signal.
struct SessionHandle {
    pc: Arc<RTCPeerConnection>,
    pump_shutdown: watch::Sender<bool>,
}

/// The real `WebRtcController`: one `RTCPeerConnection` + sendonly Opus track +
/// Opus pump per WS session. Publishes the answer + ICE candidates on the bus.
pub struct WebrtcController {
    /// Source of decoded PCM frames (the capture broadcast). Each session
    /// subscribes its own pump.
    pcm_bus: broadcast::Sender<Vec<f32>>,
    /// To publish `SystemEvent::Webrtc*` (answer + ICE) back to the owning WS.
    state: AppStateHandle,
    sessions: Mutex<HashMap<Uuid, SessionHandle>>,
}

impl WebrtcController {
    pub fn new(pcm_bus: broadcast::Sender<Vec<f32>>, state: AppStateHandle) -> Self {
        Self {
            pcm_bus,
            state,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Build the webrtc API with default codecs + interceptors.
    fn build_api() -> Result<webrtc::api::API, String> {
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .map_err(|e| e.to_string())?;
        let mut registry = Registry::new();
        registry = register_default_interceptors(registry, &mut media_engine)
            .map_err(|e| e.to_string())?;
        Ok(APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .build())
    }
}

impl WebRtcController for WebrtcController {
    async fn handle_offer(&self, session: Uuid, offer_sdp: String) -> Result<String, String> {
        // A re-offer on the same session must not orphan the previous PC + pump:
        // tear down any existing session for this UUID before building a new one.
        self.stop(session).await;

        let api = Self::build_api()?;
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec![STUN_URL.to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let pc = Arc::new(
            api.new_peer_connection(config)
                .await
                .map_err(|e| e.to_string())?,
        );

        // Sendonly Opus track (PT 111 negotiated via the offer; 48000/2ch).
        let track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: MIME_TYPE_OPUS.to_string(),
                clock_rate: 48_000,
                channels: 2,
                ..Default::default()
            },
            "audio".to_string(),
            "soundsync".to_string(),
        ));
        pc.add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
            .await
            .map_err(|e| e.to_string())?;

        // Local ICE candidates -> event bus (filtered to this session in W-3d).
        let ice_state = self.state.clone();
        pc.on_ice_candidate(Box::new(move |candidate| {
            let ice_state = ice_state.clone();
            Box::pin(async move {
                if let Some(c) = candidate {
                    if let Ok(init) = c.to_json() {
                        ice_state.publish(SystemEvent::WebrtcIceCandidate {
                            session,
                            candidate: init.candidate,
                            sdp_mid: init.sdp_mid,
                            sdp_mline_index: init.sdp_mline_index,
                        });
                    }
                }
            })
        }));

        // Offer -> answer.
        let offer = RTCSessionDescription::offer(offer_sdp).map_err(|e| e.to_string())?;
        pc.set_remote_description(offer)
            .await
            .map_err(|e| e.to_string())?;
        let answer = pc.create_answer(None).await.map_err(|e| e.to_string())?;
        let answer_sdp = answer.sdp.clone();
        pc.set_local_description(answer)
            .await
            .map_err(|e| e.to_string())?;

        // Spawn the per-session Opus pump feeding the track.
        let encoder = Arc::new(StdMutex::new(
            OpusEncoder::new().map_err(|e| e.to_string())?,
        ));
        let (pump_shutdown, shutdown_rx) = watch::channel(false);
        let rx = self.pcm_bus.subscribe();
        let sink = WebrtcRtpSink::new(track, new_ssrc());
        tokio::spawn(async move {
            run_opus_pump(rx, encoder, &sink, shutdown_rx).await;
        });

        self.sessions
            .lock()
            .await
            .insert(session, SessionHandle { pc, pump_shutdown });
        Ok(answer_sdp)
    }

    async fn add_ice(&self, session: Uuid, candidate: IceCandidate) -> Result<(), String> {
        // Clone the PC handle out and drop the map lock before the network-touching
        // await, so one session's ICE doesn't block offers/stops for others.
        let pc = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(&session)
                .map(|h| h.pc.clone())
                .ok_or_else(|| "no such session".to_string())?
        };
        pc.add_ice_candidate(RTCIceCandidateInit {
            candidate: candidate.candidate,
            sdp_mid: candidate.sdp_mid,
            sdp_mline_index: candidate.sdp_mline_index,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())
    }

    async fn stop(&self, session: Uuid) {
        let handle = self.sessions.lock().await.remove(&session);
        if let Some(handle) = handle {
            let _ = handle.pump_shutdown.send(true);
            let _ = handle.pc.close().await;
        }
    }

    async fn stop_all(&self) {
        // Drain the whole map under the lock, then close each PC + stop its pump.
        let handles: Vec<SessionHandle> =
            self.sessions.lock().await.drain().map(|(_, h)| h).collect();
        for handle in handles {
            let _ = handle.pump_shutdown.send(true);
            let _ = handle.pc.close().await;
        }
    }
}

/// A fresh SSRC per stream (avoids RTP stream collisions on the PC). SSRC only
/// needs to be unique per stream, not random-quality — derive it from a v4 UUID.
fn new_ssrc() -> u32 {
    Uuid::new_v4().as_u128() as u32
}
