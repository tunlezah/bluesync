//! axum HTTP/WebSocket server (thin glue; the state + protocol logic it wires
//! is unit-tested in `state` and `web::ws`). Same-origin SPA serving, so no CORS
//! (more secure than the legacy `Any`); the bind address is configurable.
use crate::audio::eq_controller::EqCommand;
use crate::bluetooth::avrcp::TransportCommand;
use crate::bluetooth::pairing::PairingCommand;
use crate::bluetooth::service::BtCommand;
use crate::daemon::DaemonCommand;
use crate::output::OutputCommand;
use crate::state::AppStateHandle;
use crate::sys::supervisor::wait_for_shutdown;
use crate::web::webrtc::command::WebRtcCommand;
use crate::web::webrtc::control::webrtc_out_for_session;
use crate::web::ws::{event_to_ws_message, WsInMessage, WsOutMessage};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Duration, Instant};
use tower_http::services::ServeDir;
use uuid::Uuid;

/// WS keepalive interval: how often the server sends a Ping (NF-7).
const WS_PING_INTERVAL: Duration = Duration::from_secs(15);
/// WS idle deadline: if no inbound frame (Text/Binary/Ping/Pong/Close) arrives
/// within this window the connection is considered half-open and torn down. Set
/// to a little over 2× the ping interval so a single dropped Pong is tolerated.
const WS_IDLE_DEADLINE: Duration = Duration::from_secs(35);

/// Pure decision helper (NF-7): has the WS connection been idle past `deadline`?
/// `last_seen` is the instant of the most recent inbound frame; `now` is the
/// current instant. Returns true once the gap strictly exceeds `deadline`.
fn ws_idle_timed_out(last_seen: Instant, now: Instant, deadline: Duration) -> bool {
    now.saturating_duration_since(last_seen) > deadline
}

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind (e.g. `127.0.0.1:8080`).
    pub bind: SocketAddr,
    /// Directory of built SPA assets to serve at `/`.
    pub web_dir: PathBuf,
}

/// Shared axum state: app state handle + the WebRTC command sender.
#[derive(Clone)]
pub struct WebContext {
    pub state: AppStateHandle,
    pub webrtc_tx: mpsc::Sender<WebRtcCommand>,
    pub pairing_tx: mpsc::Sender<PairingCommand>,
    pub media_tx: mpsc::Sender<TransportCommand>,
    /// EQ controller command channel; `None` if EQ support is not compiled in.
    pub eq_tx: Option<mpsc::Sender<EqCommand>>,
    /// Bluetooth device-command channel (connect/disconnect/forget/rename).
    pub bt_tx: mpsc::Sender<BtCommand>,
    /// Daemon command channel (ReassertRouting / Identify).
    pub daemon_tx: mpsc::Sender<DaemonCommand>,
    /// Output controller command channel; `None` if not yet wired.
    pub output_tx: Option<mpsc::Sender<OutputCommand>>,
    /// Process shutdown signal. Per-connection WS handlers observe it so an idle
    /// client cannot hang the graceful drain (W0.2).
    pub shutdown: watch::Receiver<bool>,
    /// Bounds concurrent `/api/stream` parec|ffmpeg pipelines (NF-37). A permit
    /// is held for the lifetime of each stream body; exhaustion returns 503.
    pub stream_limit: std::sync::Arc<tokio::sync::Semaphore>,
}

/// Build the router. `/api/status` returns the snapshot; `/ws/status` upgrades to
/// a WebSocket; everything else is served from the SPA dir (index.html fallback).
pub fn router(ctx: WebContext, web_dir: PathBuf) -> Router {
    let spa = ServeDir::new(&web_dir).append_index_html_on_directories(true);
    Router::new()
        .route("/api/status", get(status))
        .route("/ws/status", get(ws_upgrade))
        .route("/api/stream/audio.aac", get(crate::web::stream::stream_aac))
        .route("/api/stream/audio.mp3", get(crate::web::stream::stream_mp3))
        .route(
            "/api/pair",
            axum::routing::post(crate::web::pairing::open_pairing),
        )
        .route(
            "/api/pair/close",
            axum::routing::post(crate::web::pairing::close_pairing),
        )
        .route(
            "/api/media/:action",
            axum::routing::post(crate::web::media::media_command),
        )
        .route(
            "/api/eq",
            get(crate::web::eq::get_eq).post(crate::web::eq::post_eq),
        )
        .route(
            "/api/settings/name",
            axum::routing::post(crate::web::settings::set_name),
        )
        .route(
            "/api/bluetooth/:addr/:action",
            axum::routing::post(crate::web::bt_cmd::device_command),
        )
        .route(
            "/api/admin/restart",
            axum::routing::post(crate::web::admin::admin_restart),
        )
        .route(
            "/api/admin/identify",
            axum::routing::post(crate::web::admin::admin_identify),
        )
        .route("/api/outputs", get(crate::web::output::get_outputs))
        .route(
            "/api/output",
            axum::routing::post(crate::web::output::set_output),
        )
        .fallback_service(spa)
        .with_state(ctx)
}

async fn status(State(ctx): State<WebContext>) -> impl IntoResponse {
    Json(ctx.state.snapshot().await)
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(ctx): State<WebContext>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_connection(socket, ctx))
}

/// Per-connection: send the initial snapshot, then dispatch WebRTC signalling
/// inbound and forward `SystemEvent`s outbound (broadcast + per-session ICE).
async fn ws_connection(mut socket: WebSocket, ctx: WebContext) {
    let session = Uuid::new_v4();

    // Per-connection view of the process shutdown signal so an idle client can't
    // hold the graceful drain open (W0.2).
    let mut shutdown = ctx.shutdown.clone();

    // Subscribe before snapshot so no update is missed while connecting.
    // Worst case the client receives one duplicate event it re-renders idempotently.
    let mut events = ctx.state.subscribe();

    // Initial snapshot (taken after subscribing to close the snapshot/subscribe gap).
    if let Ok(json) = serde_json::to_string(&WsOutMessage::StateSnapshot {
        data: Box::new(ctx.state.snapshot().await),
    }) {
        if socket.send(Message::Text(json)).await.is_err() {
            return;
        }
    }

    // WS keepalive (NF-7): periodically Ping and enforce an idle deadline so a
    // half-open client (TCP wedged but ICE nominally connected) is reaped via
    // the existing Stop teardown rather than leaking the PC + Opus pump. Any
    // inbound frame (Text/Binary/Ping/Pong/Close) refreshes `last_seen`.
    let mut last_seen = Instant::now();
    let mut keepalive = tokio::time::interval(WS_PING_INTERVAL);
    // The first tick fires immediately; skip it so we don't Ping before the
    // client has even settled.
    keepalive.tick().await;
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => break,
            _ = keepalive.tick() => {
                if ws_idle_timed_out(last_seen, Instant::now(), WS_IDLE_DEADLINE) {
                    // Stale: break to the teardown path below (sends Stop).
                    break;
                }
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    break;
                }
            }
            ev = events.recv() => match ev {
                Ok(ev) => {
                    // Broadcast events (e.g. spectrum).
                    if let Some(msg) = event_to_ws_message(&ev) {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if socket.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                    // Per-session WebRTC events (ICE candidates for this session).
                    if let Some(msg) = webrtc_out_for_session(&ev, session) {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            if socket.send(Message::Text(json)).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    // Any inbound frame proves the peer is alive (NF-7).
                    last_seen = Instant::now();
                    match msg {
                    Message::Text(t) => {
                    match serde_json::from_str::<WsInMessage>(&t) {
                        Ok(WsInMessage::WebrtcOffer { data }) => {
                            let (rtx, rrx) = oneshot::channel();
                            let _ = ctx.webrtc_tx.send(WebRtcCommand::Offer {
                                session,
                                sdp: data.sdp,
                                reply: rtx,
                            }).await;
                            if let Ok(Ok(answer)) = rrx.await {
                                if let Ok(json) = serde_json::to_string(&WsOutMessage::WebrtcAnswer {
                                    data: crate::web::ws::AnswerData { sdp: answer },
                                }) {
                                    if socket.send(Message::Text(json)).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Ok(WsInMessage::WebrtcIceCandidate { data }) => {
                            let _ = ctx.webrtc_tx.send(WebRtcCommand::Ice {
                                session,
                                candidate: data,
                            }).await;
                        }
                        Ok(WsInMessage::WebrtcStop) => {
                            let _ = ctx.webrtc_tx.send(WebRtcCommand::Stop { session }).await;
                        }
                        Err(_) => { /* ignore parse errors */ }
                    }
                    }
                    Message::Close(_) => {
                        let _ = ctx.webrtc_tx.send(WebRtcCommand::Stop { session }).await;
                        return;
                    }
                    // Ping/Pong/Binary already refreshed `last_seen` above.
                    _ => { /* ignore other message types */ }
                    }
                }
                None | Some(Err(_)) => {
                    let _ = ctx.webrtc_tx.send(WebRtcCommand::Stop { session }).await;
                    return;
                }
            },
        }
    }

    // On any return path via break, send Stop so the PC is torn down on disconnect.
    let _ = ctx.webrtc_tx.send(WebRtcCommand::Stop { session }).await;
}

/// Bind and serve until the process ends or shutdown is signalled. The server
/// stops accepting new connections and drains in-flight ones once `ctx.shutdown`
/// flips, so SIGTERM returns promptly instead of hanging on the accept loop
/// (W0.2). Per-connection WS handlers observe the same signal, so an idle client
/// cannot keep the drain open.
pub async fn serve(config: ServerConfig, ctx: WebContext) -> std::io::Result<()> {
    let mut shutdown = ctx.shutdown.clone();
    let app = router(ctx, config.web_dir);
    let listener = bind_with_retry(config.bind, &mut shutdown).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            wait_for_shutdown(&mut shutdown).await;
        })
        .await
}

/// Max bind attempts before giving up (NF-15).
const BIND_MAX_ATTEMPTS: usize = 5;
/// Delay between bind attempts (NF-15).
const BIND_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Bind the listener, tolerating a transient `AddrInUse` after a fast restart
/// (a still-exiting prior instance briefly holding the listening socket; NF-15).
/// Retries up to [`BIND_MAX_ATTEMPTS`] with a short delay, racing each delay
/// against shutdown so a SIGTERM mid-retry returns promptly. `SO_REUSEADDR` is
/// deliberately NOT used: on Linux it does not let a second process steal a
/// LISTENING socket still held by the old instance, so the bounded retry — not
/// REUSEADDR — is the load-bearing fix. Non-`AddrInUse` errors fail immediately.
async fn bind_with_retry(
    addr: SocketAddr,
    shutdown: &mut watch::Receiver<bool>,
) -> std::io::Result<tokio::net::TcpListener> {
    let mut attempt = 0usize;
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                attempt += 1;
                if attempt >= BIND_MAX_ATTEMPTS {
                    return Err(e);
                }
                tokio::select! {
                    _ = tokio::time::sleep(BIND_RETRY_DELAY) => continue,
                    _ = wait_for_shutdown(shutdown) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_idle_not_timed_out_within_deadline() {
        let start = Instant::now();
        let deadline = Duration::from_secs(35);
        // Exactly at the deadline is NOT timed out (strictly-greater semantics).
        assert!(!ws_idle_timed_out(start, start + deadline, deadline));
        // Comfortably inside the window.
        assert!(!ws_idle_timed_out(
            start,
            start + Duration::from_secs(10),
            deadline
        ));
    }

    #[test]
    fn ws_idle_timed_out_past_deadline() {
        let start = Instant::now();
        let deadline = Duration::from_secs(35);
        assert!(ws_idle_timed_out(
            start,
            start + deadline + Duration::from_millis(1),
            deadline
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn bind_with_retry_succeeds_on_free_port() {
        let (_tx, mut rx) = watch::channel(false);
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = bind_with_retry(addr, &mut rx).await.unwrap();
        // Port 0 => the OS picked an ephemeral port; bind succeeded first try.
        assert!(listener.local_addr().unwrap().port() != 0);
    }

    #[tokio::test(start_paused = true)]
    async fn bind_with_retry_gives_up_after_max_attempts_on_addr_in_use() {
        // Hold a real listener so the same addr is genuinely AddrInUse.
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap();
        let (_tx, mut rx) = watch::channel(false);
        let err = bind_with_retry(addr, &mut rx).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[tokio::test(start_paused = true)]
    async fn bind_with_retry_returns_promptly_on_shutdown_mid_retry() {
        let held = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = held.local_addr().unwrap();
        let (tx, mut rx) = watch::channel(false);
        // Signal shutdown before the call so the first retry sleep loses the race
        // immediately and the function returns the AddrInUse error.
        tx.send(true).unwrap();
        let err = bind_with_retry(addr, &mut rx).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }
}
