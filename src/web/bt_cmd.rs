//! `POST /api/bluetooth/:addr/:action` — per-device connect/disconnect/forget.
//!
//! The action parser is pure + tested. The axum handler is thin glue that
//! `try_send`s a `BtCommand` (202 / 400 / 503), mirroring the media pattern.
use crate::bluetooth::service::BtCommand;
use crate::web::server::WebContext;
use axum::extract::{Path, State};
use axum::http::StatusCode;

/// Map a URL-encoded address + action string to a `BtCommand`, or return
/// `Err(StatusCode)` for unknown actions.
pub fn action_to_btcommand(addr: String, action: &str) -> Result<BtCommand, StatusCode> {
    match action {
        "connect" => Ok(BtCommand::Connect(addr)),
        "disconnect" => Ok(BtCommand::Disconnect(addr)),
        "forget" => Ok(BtCommand::Forget(addr)),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

/// `POST /api/bluetooth/:addr/:action` — issue a device command.
///
/// Returns `202 Accepted` on success, `400 Bad Request` for unknown actions,
/// `503 Service Unavailable` if the channel is full or closed.
pub async fn device_command(
    State(ctx): State<WebContext>,
    Path((addr, action)): Path<(String, String)>,
) -> StatusCode {
    let cmd = match action_to_btcommand(addr, &action) {
        Ok(c) => c,
        Err(status) => return status,
    };
    match ctx.bt_tx.try_send(cmd) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::service::BtCommand;

    fn addr() -> String {
        "AA:BB:CC:DD:EE:FF".to_string()
    }

    #[test]
    fn connect_maps_to_connect_command() {
        let cmd = action_to_btcommand(addr(), "connect").unwrap();
        assert!(matches!(cmd, BtCommand::Connect(a) if a == addr()));
    }

    #[test]
    fn disconnect_maps_to_disconnect_command() {
        let cmd = action_to_btcommand(addr(), "disconnect").unwrap();
        assert!(matches!(cmd, BtCommand::Disconnect(a) if a == addr()));
    }

    #[test]
    fn forget_maps_to_forget_command() {
        let cmd = action_to_btcommand(addr(), "forget").unwrap();
        assert!(matches!(cmd, BtCommand::Forget(a) if a == addr()));
    }

    #[test]
    fn unknown_action_returns_bad_request() {
        let err = action_to_btcommand(addr(), "teleport").unwrap_err();
        assert_eq!(err, StatusCode::BAD_REQUEST);
    }
}
