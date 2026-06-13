//! `/api/eq` HTTP handler — GET (read EQ state) + POST (update gains / enable).
use crate::audio::eq_controller::EqCommand;
use crate::dsp::eq::{MAX_GAIN_DB, MIN_GAIN_DB, NUM_BANDS};
use crate::web::server::WebContext;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;

/// `POST /api/eq` request body.
#[derive(Debug, Deserialize)]
pub struct EqUpdateRequest {
    /// Optional enable/disable toggle.
    pub enabled: Option<bool>,
    /// Optional 10-element gains array (only `gain_db` is mutable per band).
    pub gains: Option<Vec<f32>>,
}

/// `GET /api/eq` — return the current EQ snapshot.
pub async fn get_eq(State(ctx): State<WebContext>) -> impl IntoResponse {
    Json(ctx.state.snapshot().await.eq)
}

/// `POST /api/eq` — apply gains / enable / disable.
///
/// Returns 202 Accepted on success, 400 on bad input, 503 if the controller
/// channel is full.
pub async fn post_eq(
    State(ctx): State<WebContext>,
    Json(body): Json<EqUpdateRequest>,
) -> impl IntoResponse {
    let eq_tx = match &ctx.eq_tx {
        Some(tx) => tx.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "EQ controller not available",
            )
                .into_response()
        }
    };

    // Validate + send UpdateGains if gains provided.
    if let Some(gains_vec) = body.gains {
        match parse_gains(&gains_vec) {
            Ok(gains) => {
                if eq_tx.try_send(EqCommand::UpdateGains(gains)).is_err() {
                    return (StatusCode::SERVICE_UNAVAILABLE, "EQ controller busy").into_response();
                }
            }
            Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
        }
    }

    // Send SetEnabled if the flag is provided.
    if let Some(enabled) = body.enabled {
        if eq_tx.try_send(EqCommand::SetEnabled(enabled)).is_err() {
            return (StatusCode::SERVICE_UNAVAILABLE, "EQ controller busy").into_response();
        }
    }

    StatusCode::ACCEPTED.into_response()
}

/// Parse and clamp a Vec<f32> into a [f32; NUM_BANDS] array.
///
/// Returns `Err` with a descriptive message if the length is wrong.
/// Gains are clamped to [`MIN_GAIN_DB`, `MAX_GAIN_DB`] — out-of-range values
/// are silently clamped rather than rejected (matches the controller's own
/// clamping in `EqCommand::UpdateGains`).
pub fn parse_gains(gains: &[f32]) -> Result<[f32; NUM_BANDS], &'static str> {
    if gains.len() != NUM_BANDS {
        return Err("gains must have exactly 10 elements");
    }
    let mut arr = [0.0_f32; NUM_BANDS];
    for (i, &g) in gains.iter().enumerate() {
        arr[i] = g.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
    }
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gains_accepts_valid_input() {
        let v: Vec<f32> = vec![0.0; NUM_BANDS];
        let arr = parse_gains(&v).unwrap();
        assert_eq!(arr.len(), NUM_BANDS);
        assert!(arr.iter().all(|&g| g == 0.0));
    }

    #[test]
    fn parse_gains_clamps_out_of_range() {
        let mut v = vec![0.0_f32; NUM_BANDS];
        v[0] = 20.0; // over max
        v[9] = -20.0; // under min
        let arr = parse_gains(&v).unwrap();
        assert_eq!(arr[0], MAX_GAIN_DB);
        assert_eq!(arr[9], MIN_GAIN_DB);
    }

    #[test]
    fn parse_gains_rejects_wrong_length() {
        assert!(parse_gains(&[0.0; 9]).is_err());
        assert!(parse_gains(&[0.0; 11]).is_err());
        assert!(parse_gains(&[]).is_err());
    }

    #[test]
    fn eq_update_request_deserializes_partial() {
        // Only enabled — no gains
        let json = r#"{"enabled":true}"#;
        let req: EqUpdateRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.enabled, Some(true));
        assert!(req.gains.is_none());

        // Only gains — no enabled
        let gains_json = format!(
            r#"{{"gains":{}}}"#,
            serde_json::to_string(&vec![0.0_f32; NUM_BANDS]).unwrap()
        );
        let req2: EqUpdateRequest = serde_json::from_str(&gains_json).unwrap();
        assert!(req2.enabled.is_none());
        assert_eq!(req2.gains.as_ref().unwrap().len(), NUM_BANDS);
    }

    #[test]
    fn eq_update_request_deserializes_both() {
        let json = format!(
            r#"{{"enabled":false,"gains":{}}}"#,
            serde_json::to_string(&vec![1.0_f32; NUM_BANDS]).unwrap()
        );
        let req: EqUpdateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.enabled, Some(false));
        assert!(req.gains.is_some());
    }
}
