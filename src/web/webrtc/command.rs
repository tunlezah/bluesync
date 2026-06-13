//! WebRTC command channel: decouples the WS server from the (non-object-safe)
//! `WebRtcController` trait. A single loop owns the concrete controller.
use crate::sys::supervisor::wait_for_shutdown;
use crate::web::webrtc::control::WebRtcController;
use crate::web::ws::IceCandidate;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

/// Commands from WS connections to the WebRTC controller task.
pub enum WebRtcCommand {
    /// Browser offer; the answer SDP is returned via `reply`.
    Offer {
        session: Uuid,
        sdp: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    /// A remote ICE candidate for `session`.
    Ice {
        session: Uuid,
        candidate: IceCandidate,
    },
    /// Tear down `session` (explicit stop or WS disconnect).
    Stop { session: Uuid },
}

/// Own the controller and service commands until the channel closes or
/// `shutdown` is signalled. On shutdown the loop tears down every active session
/// (`stop_all`) so no peer connection / Opus pump survives the graceful drain
/// (W0.2).
pub async fn webrtc_command_loop<C: WebRtcController>(
    controller: C,
    mut rx: mpsc::Receiver<WebRtcCommand>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let cmd = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            cmd = rx.recv() => cmd,
        };
        let Some(cmd) = cmd else { break };
        match cmd {
            WebRtcCommand::Offer {
                session,
                sdp,
                reply,
            } => {
                let _ = reply.send(controller.handle_offer(session, sdp).await);
            }
            WebRtcCommand::Ice { session, candidate } => {
                let _ = controller.add_ice(session, candidate).await;
            }
            WebRtcCommand::Stop { session } => controller.stop(session).await,
        }
    }
    controller.stop_all().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::webrtc::control::FakeWebRtcController;
    use crate::web::ws::IceCandidate;
    use tokio::sync::{mpsc, oneshot, watch};
    use uuid::Uuid;

    #[tokio::test]
    async fn offer_replies_with_answer_then_ice_and_stop_dispatch() {
        let controller = FakeWebRtcController::default();
        let (tx, rx) = mpsc::channel(8);
        let (_sd_tx, sd_rx) = watch::channel(false);
        let loop_handle = tokio::spawn(async move {
            webrtc_command_loop(controller, rx, sd_rx).await;
        });

        let s = Uuid::from_u128(1);
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(WebRtcCommand::Offer {
            session: s,
            sdp: "off".into(),
            reply: reply_tx,
        })
        .await
        .unwrap();
        assert_eq!(reply_rx.await.unwrap().unwrap(), "fake-answer");

        tx.send(WebRtcCommand::Ice {
            session: s,
            candidate: IceCandidate {
                candidate: "c".into(),
                sdp_mid: None,
                sdp_mline_index: None,
            },
        })
        .await
        .unwrap();
        tx.send(WebRtcCommand::Stop { session: s }).await.unwrap();

        drop(tx); // close -> loop ends
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_signal_drains_all_sessions() {
        let controller = FakeWebRtcController::default();
        let observer = controller.clone(); // shares the call log
        let (_tx, rx) = mpsc::channel::<WebRtcCommand>(8);
        let (sd_tx, sd_rx) = watch::channel(false);
        let loop_handle = tokio::spawn(async move {
            webrtc_command_loop(controller, rx, sd_rx).await;
        });

        // Signal shutdown; the loop should exit and call stop_all exactly once.
        sd_tx.send(true).unwrap();
        loop_handle.await.unwrap();

        assert_eq!(observer.calls(), vec!["stop_all".to_string()]);
    }
}
