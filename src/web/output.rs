//! `/api/outputs` GET + `/api/output` POST handlers (multi-output-design.md).
//!
//! `GET /api/outputs` — return current `OutputState` (active + available).
//! `POST /api/output` body `{ "type": "none"|"soundcard"|"airplay"|"chromecast", "id": "<id>" }`
//!   → parse → `OutputCommand` → `output_tx.try_send` → 202/400/503.
//!
//! The parse is a pure function so it is independently unit-tested.

use crate::output::{OutputCommand, OutputKind};
use crate::web::server::WebContext;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

/// `POST /api/output` request body.
#[derive(Debug, Deserialize)]
pub struct SetOutputRequest {
    /// One of `"none"`, `"soundcard"`, `"airplay"`, `"chromecast"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Target device id (ignored when `type` is `"none"`).
    pub id: Option<String>,
}

/// Parse a `SetOutputRequest` into an `OutputCommand`.
///
/// Returns:
/// - `Ok(OutputCommand::None)`               for `type == "none"`
/// - `Ok(OutputCommand::Select { kind, id })` for valid kind + non-empty id
/// - `Err(&'static str)`                      for unknown kind or missing/empty id
pub fn parse_output_request(kind: &str, id: Option<&str>) -> Result<OutputCommand, &'static str> {
    match kind {
        "none" => Ok(OutputCommand::None),
        "soundcard" | "airplay" | "chromecast" => {
            let id = match id {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => return Err("id is required for non-none output type"),
            };
            let output_kind = match kind {
                "soundcard" => OutputKind::Soundcard,
                "airplay" => OutputKind::Airplay,
                "chromecast" => OutputKind::Chromecast,
                _ => unreachable!(),
            };
            Ok(OutputCommand::Select {
                kind: output_kind,
                id,
            })
        }
        _ => Err("unknown output type"),
    }
}

/// `GET /api/outputs` — return the current output state (active + available).
pub async fn get_outputs(State(ctx): State<WebContext>) -> impl IntoResponse {
    Json(ctx.state.snapshot().await.output)
}

/// `POST /api/output` — select or clear an output device.
///
/// Returns 202 Accepted on success, 400 on bad input, 503 if the controller
/// channel is full or unavailable.
pub async fn set_output(
    State(ctx): State<WebContext>,
    Json(body): Json<SetOutputRequest>,
) -> impl IntoResponse {
    let output_tx = match &ctx.output_tx {
        Some(tx) => tx.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "output controller not available",
            )
                .into_response()
        }
    };

    let cmd = match parse_output_request(&body.kind, body.id.as_deref()) {
        Ok(c) => c,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };

    match output_tx.try_send(cmd) {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "output controller busy").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_request_none_type() {
        let cmd = parse_output_request("none", None).unwrap();
        assert!(matches!(cmd, OutputCommand::None));
    }

    #[test]
    fn parse_output_request_none_type_ignores_id() {
        // id is ignored when type is "none"
        let cmd = parse_output_request("none", Some("anything")).unwrap();
        assert!(matches!(cmd, OutputCommand::None));
    }

    #[test]
    fn parse_output_request_soundcard() {
        let cmd = parse_output_request("soundcard", Some("alsa_output.usb-FiiO_BTR3")).unwrap();
        match cmd {
            OutputCommand::Select { kind, id } => {
                assert_eq!(kind, OutputKind::Soundcard);
                assert_eq!(id, "alsa_output.usb-FiiO_BTR3");
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn parse_output_request_airplay() {
        let cmd = parse_output_request("airplay", Some("HomePod@homepod.local")).unwrap();
        match cmd {
            OutputCommand::Select { kind, id } => {
                assert_eq!(kind, OutputKind::Airplay);
                assert_eq!(id, "HomePod@homepod.local");
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn parse_output_request_chromecast() {
        let cmd = parse_output_request("chromecast", Some("d122a7025b65")).unwrap();
        match cmd {
            OutputCommand::Select { kind, id } => {
                assert_eq!(kind, OutputKind::Chromecast);
                assert_eq!(id, "d122a7025b65");
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn parse_output_request_unknown_kind_is_error() {
        assert!(parse_output_request("bluetooth", Some("xx")).is_err());
        assert!(parse_output_request("", None).is_err());
    }

    #[test]
    fn parse_output_request_missing_id_is_error() {
        assert!(parse_output_request("soundcard", None).is_err());
    }

    #[test]
    fn parse_output_request_empty_id_is_error() {
        assert!(parse_output_request("airplay", Some("")).is_err());
    }
}
