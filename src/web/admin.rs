//! Admin command surface: `POST /api/admin/restart` (re-assert audio routing)
//! and `POST /api/admin/identify` (play a brief test tone). Both handlers
//! `try_send` a `DaemonCommand` (202/503), mirroring the pairing/media pattern.
use crate::daemon::DaemonCommand;
use crate::web::server::WebContext;
use axum::extract::State;
use axum::http::StatusCode;

/// `POST /api/admin/restart` — re-assert audio routing (recreate null sink +
/// set default sink). Useful when PipeWire state has drifted.
///
/// Returns `202 Accepted` on success, `503 Service Unavailable` if the daemon
/// channel is full or closed.
pub async fn admin_restart(State(ctx): State<WebContext>) -> StatusCode {
    match ctx.daemon_tx.try_send(DaemonCommand::ReassertRouting) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// `POST /api/admin/identify` — play a brief audible test tone so the user can
/// confirm audio routing is working.
///
/// Returns `202 Accepted` on success, `503 Service Unavailable` if the daemon
/// channel is full or closed.
pub async fn admin_identify(State(ctx): State<WebContext>) -> StatusCode {
    match ctx.daemon_tx.try_send(DaemonCommand::Identify) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
