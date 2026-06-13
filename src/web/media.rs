//! The media transport command surface (bluetooth-design.md §6): `POST
//! /api/media/<action>` (play/pause/next/previous/stop) sends a
//! `TransportCommand` to the media manager via `WebContext.media_tx`. The action
//! parsing is pure + tested; the axum handler is thin glue.
use crate::bluetooth::avrcp::TransportCommand;
use crate::web::server::WebContext;
use axum::extract::{Path, State};
use axum::http::StatusCode;

/// Parse a `/api/media/<action>` token into a `TransportCommand`.
pub fn parse_action(action: &str) -> Option<TransportCommand> {
    TransportCommand::from_action(action)
}

/// `POST /api/media/:action` — issue a transport command.
///
/// Uses `try_send` (non-blocking): returns `202 Accepted` on success, or
/// `503 Service Unavailable` if the channel is full or closed — the HTTP
/// handler never suspends waiting for the consumer.
pub async fn media_command(
    State(ctx): State<WebContext>,
    Path(action): Path<String>,
) -> StatusCode {
    let Some(cmd) = parse_action(&action) else {
        return StatusCode::BAD_REQUEST;
    };
    match ctx.media_tx.try_send(cmd) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::avrcp::TransportCommand;

    #[test]
    fn known_actions_parse() {
        assert_eq!(parse_action("play"), Some(TransportCommand::Play));
        assert_eq!(parse_action("stop"), Some(TransportCommand::Stop));
    }

    #[test]
    fn unknown_action_is_none() {
        assert_eq!(parse_action("teleport"), None);
    }
}
