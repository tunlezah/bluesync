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
use tower_http::services::ServeDir;
use uuid::Uuid;

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
    loop {
        tokio::select! {
            _ = wait_for_shutdown(&mut shutdown) => break,
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
                Some(Ok(Message::Text(t))) => {
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
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    let _ = ctx.webrtc_tx.send(WebRtcCommand::Stop { session }).await;
                    return;
                }
                Some(Ok(_)) => { /* ignore other message types */ }
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
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            wait_for_shutdown(&mut shutdown).await;
        })
        .await
}
