//! The WebRtcController seam (all webrtc-rs lives behind it — real impl in
//! `webrtc_rs.rs`) + per-session event filtering (AUD-053).
use crate::state::SystemEvent;
use crate::web::ws::{AnswerData, IceCandidate, WsOutMessage};
use std::future::Future;
use uuid::Uuid;

/// Drives WebRTC sessions. The real impl wraps webrtc-rs; tests use a fake.
pub trait WebRtcController: Send + Sync {
    /// Handle a browser offer; set up the PC + sendonly Opus track + pump and
    /// return the answer SDP. ICE candidates are emitted on the event bus.
    fn handle_offer(
        &self,
        session: Uuid,
        offer_sdp: String,
    ) -> impl Future<Output = Result<String, String>> + Send;
    /// Add a remote ICE candidate to the session's PC.
    fn add_ice(
        &self,
        session: Uuid,
        candidate: IceCandidate,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Tear down a session.
    fn stop(&self, session: Uuid) -> impl Future<Output = ()> + Send;
    /// Tear down every active session (graceful shutdown drain, W0.2). After
    /// this returns no peer connections or pumps remain.
    fn stop_all(&self) -> impl Future<Output = ()> + Send;
}

/// Map a webrtc `SystemEvent` to the wire message for ONE session, or `None` if
/// the event is for a different session or is not a per-session webrtc event.
/// (Non-webrtc events like spectrum are broadcast to all via `event_to_ws_message`.)
pub fn webrtc_out_for_session(event: &SystemEvent, session: Uuid) -> Option<WsOutMessage> {
    match event {
        SystemEvent::WebrtcAnswer { session: s, sdp } if *s == session => {
            Some(WsOutMessage::WebrtcAnswer {
                data: AnswerData { sdp: sdp.clone() },
            })
        }
        SystemEvent::WebrtcIceCandidate {
            session: s,
            candidate,
            sdp_mid,
            sdp_mline_index,
        } if *s == session => Some(WsOutMessage::WebrtcIceCandidate {
            data: IceCandidate {
                candidate: candidate.clone(),
                sdp_mid: sdp_mid.clone(),
                sdp_mline_index: *sdp_mline_index,
            },
        }),
        _ => None,
    }
}

/// Test double: records calls; `handle_offer` returns a canned answer. The call
/// log is behind a shared `Arc` so a test can keep observing it after the
/// controller is moved into `webrtc_command_loop`.
#[cfg(test)]
#[derive(Default, Clone)]
pub struct FakeWebRtcController {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
impl FakeWebRtcController {
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl WebRtcController for FakeWebRtcController {
    async fn handle_offer(&self, _session: Uuid, _offer_sdp: String) -> Result<String, String> {
        self.calls.lock().unwrap().push("offer".to_string());
        Ok("fake-answer".to_string())
    }
    async fn add_ice(&self, _session: Uuid, _candidate: IceCandidate) -> Result<(), String> {
        self.calls.lock().unwrap().push("ice".to_string());
        Ok(())
    }
    async fn stop(&self, _session: Uuid) {
        self.calls.lock().unwrap().push("stop".to_string());
    }
    async fn stop_all(&self) {
        self.calls.lock().unwrap().push("stop_all".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::SystemEvent;
    use uuid::Uuid;

    #[test]
    fn answer_filtered_to_owning_session() {
        let me = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let ev = SystemEvent::WebrtcAnswer {
            session: me,
            sdp: "v=0".into(),
        };
        // For my session -> Some(answer); for another -> None.
        match webrtc_out_for_session(&ev, me) {
            Some(WsOutMessage::WebrtcAnswer { data }) => assert_eq!(data.sdp, "v=0"),
            _ => panic!("expected answer"),
        }
        assert!(webrtc_out_for_session(&ev, other).is_none());
    }

    #[test]
    fn ice_filtered_and_mapped_with_casing() {
        let me = Uuid::from_u128(7);
        let ev = SystemEvent::WebrtcIceCandidate {
            session: me,
            candidate: "cand".into(),
            sdp_mid: Some("0".into()),
            sdp_mline_index: Some(0),
        };
        match webrtc_out_for_session(&ev, me) {
            Some(WsOutMessage::WebrtcIceCandidate { data }) => {
                assert_eq!(data.candidate, "cand");
                assert_eq!(data.sdp_mid.as_deref(), Some("0"));
                assert_eq!(data.sdp_mline_index, Some(0));
            }
            _ => panic!("expected ice"),
        }
        assert!(webrtc_out_for_session(&ev, Uuid::from_u128(8)).is_none());
    }

    #[test]
    fn non_webrtc_event_is_not_a_session_message() {
        // SpectrumData is broadcast to all (handled by event_to_ws_message), so
        // the per-session webrtc filter returns None for it.
        assert!(webrtc_out_for_session(
            &SystemEvent::SpectrumData { bands: vec![] },
            Uuid::from_u128(1)
        )
        .is_none());
    }

    #[tokio::test]
    async fn fake_controller_records_calls() {
        let c = FakeWebRtcController::default();
        let s = Uuid::from_u128(3);
        assert_eq!(
            c.handle_offer(s, "offer".into()).await.unwrap(),
            "fake-answer"
        );
        c.add_ice(
            s,
            IceCandidate {
                candidate: "x".into(),
                sdp_mid: None,
                sdp_mline_index: None,
            },
        )
        .await
        .unwrap();
        c.stop(s).await;
        assert_eq!(
            c.calls(),
            vec!["offer".to_string(), "ice".to_string(), "stop".to_string()]
        );
    }
}
