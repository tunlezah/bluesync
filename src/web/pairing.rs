//! The `pair` command surface (bluetooth-design.md §2): `POST /api/pair` opens
//! the robust, re-openable pairing window for a bounded time; `POST /api/pair/close`
//! closes it. The window itself (`run_pairing_window`) is driven via a
//! `mpsc::Sender<PairingCommand>` on `WebContext`. The duration logic is pure +
//! tested; the axum handlers are thin glue.
use crate::bluetooth::pairing::PairingCommand;
use crate::web::server::WebContext;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use tokio::time::Duration;

/// Default pairing-window length (5 minutes; bluetooth-design.md §2).
pub const DEFAULT_PAIRING_SECS: u64 = 300;
/// Minimum window (a too-short window is unusable). `Some(0)` is treated as
/// "open at least briefly" (clamped up), not rejected.
pub const MIN_PAIRING_SECS: u64 = 30;
/// Maximum window (cap; hardened default — not indefinitely discoverable).
pub const MAX_PAIRING_SECS: u64 = 3600;

/// Build the `Open` command from an optional requested duration in seconds,
/// applying the default and clamping to `[MIN, MAX]`.
pub fn open_command(seconds: Option<u64>) -> PairingCommand {
    let secs = match seconds {
        None => DEFAULT_PAIRING_SECS,
        Some(s) => s.clamp(MIN_PAIRING_SECS, MAX_PAIRING_SECS),
    };
    PairingCommand::Open {
        duration: Duration::from_secs(secs),
    }
}

/// Query for `POST /api/pair?seconds=NNN` (optional).
#[derive(Debug, Deserialize)]
pub struct PairQuery {
    pub seconds: Option<u64>,
}

/// `POST /api/pair` — open (or re-open/extend) the pairing window.
///
/// Uses `try_send` (non-blocking): returns `202 Accepted` on success, or
/// `503 Service Unavailable` if the channel is full or closed.
pub async fn open_pairing(State(ctx): State<WebContext>, Query(q): Query<PairQuery>) -> StatusCode {
    match ctx.pairing_tx.try_send(open_command(q.seconds)) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// `POST /api/pair/close` — close the pairing window now.
///
/// Uses `try_send` (non-blocking): returns `202 Accepted` on success, or
/// `503 Service Unavailable` if the channel is full or closed.
pub async fn close_pairing(State(ctx): State<WebContext>) -> StatusCode {
    match ctx.pairing_tx.try_send(PairingCommand::Close) {
        Ok(()) => StatusCode::ACCEPTED,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::pairing::PairingCommand;
    use tokio::time::Duration;

    #[test]
    fn default_seconds_when_none() {
        assert_eq!(
            open_command(None),
            PairingCommand::Open {
                duration: Duration::from_secs(DEFAULT_PAIRING_SECS)
            }
        );
    }

    #[test]
    fn uses_supplied_seconds() {
        assert_eq!(
            open_command(Some(120)),
            PairingCommand::Open {
                duration: Duration::from_secs(120)
            }
        );
    }

    #[test]
    fn clamps_zero_and_excessive_to_bounds() {
        // 0 -> minimum window; absurdly large -> max.
        assert_eq!(
            open_command(Some(0)),
            PairingCommand::Open {
                duration: Duration::from_secs(MIN_PAIRING_SECS)
            }
        );
        assert_eq!(
            open_command(Some(u64::MAX)),
            PairingCommand::Open {
                duration: Duration::from_secs(MAX_PAIRING_SECS)
            }
        );
    }
}
