//! `POST /api/settings/name` — rename the Bluetooth adapter.
//!
//! The name validator is pure + tested. The axum handler is thin glue that
//! `try_send`s a `BtCommand::SetName` onto the channel (202 / 400 / 503),
//! mirroring the pairing/media pattern.
use crate::bluetooth::service::BtCommand;
use crate::web::server::WebContext;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

/// Maximum allowed device name length (bytes).
const MAX_NAME_LEN: usize = 248;

/// Inbound JSON body for `POST /api/settings/name`.
#[derive(Debug, Deserialize)]
pub struct SetNameBody {
    pub name: String,
}

/// Validate a device name: non-empty, within `MAX_NAME_LEN` bytes.
/// Returns `Ok(trimmed_name)` or `Err(reason)`.
pub fn parse_name(name: &str) -> Result<String, &'static str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("name must not be empty");
    }
    if trimmed.len() > MAX_NAME_LEN {
        return Err("name too long (max 248 bytes)");
    }
    Ok(trimmed.to_string())
}

/// `POST /api/settings/name` — rename the Bluetooth adapter.
///
/// Returns `202 Accepted` on success, `400 Bad Request` for invalid names,
/// `503 Service Unavailable` if the channel is full or closed.
pub async fn set_name(State(ctx): State<WebContext>, Json(body): Json<SetNameBody>) -> StatusCode {
    let name = match parse_name(&body.name) {
        Ok(n) => n,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    match ctx.bt_tx.try_send(BtCommand::SetName(name)) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_name_is_accepted() {
        assert_eq!(parse_name("SoundSync"), Ok("SoundSync".to_string()));
        assert_eq!(parse_name("  MyDevice  "), Ok("MyDevice".to_string()));
    }

    #[test]
    fn empty_name_is_rejected() {
        assert!(parse_name("").is_err());
        assert!(parse_name("   ").is_err());
    }

    #[test]
    fn over_long_name_is_rejected() {
        let long = "x".repeat(249);
        assert!(parse_name(&long).is_err());
    }

    #[test]
    fn max_length_name_is_accepted() {
        let at_limit = "x".repeat(248);
        assert!(parse_name(&at_limit).is_ok());
    }
}
