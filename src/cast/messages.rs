//! CASTV2 JSON payload builders and RECEIVER_STATUS parser.
//!
//! Chromecasts exchange JSON-encoded control messages on specific namespaces
//! over the `CastMessage.payload_utf8` field.  This module provides:
//!
//! - String constants for the application id and the four namespaces.
//! - Pure builder functions that return JSON strings ready to embed in a
//!   `CastMessage` (via [`super::proto::string_message`]).
//! - [`parse_launched_transport_id`] — extract the `transportId` of a launched
//!   app from a RECEIVER_STATUS payload.
//! - [`message_type`] — extract the `"type"` field for message routing.

// ─── Constants ───────────────────────────────────────────────────────────────

/// Default Media Receiver application id (the built-in Chromecast app).
pub const DEFAULT_MEDIA_RECEIVER_APP_ID: &str = "CC1AD845";

/// Transport-layer connection namespace.
pub const NS_CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";

/// Heartbeat (PING/PONG) namespace.
pub const NS_HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";

/// Receiver control namespace (LAUNCH, GET_STATUS, RECEIVER_STATUS …).
pub const NS_RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";

/// Media control namespace (LOAD, MEDIA_STATUS, STOP …).
pub const NS_MEDIA: &str = "urn:x-cast:com.google.cast.media";

// ─── Payload builders ────────────────────────────────────────────────────────

/// `{"type":"CONNECT"}` — sent on `NS_CONNECTION` to open a virtual channel.
pub fn connect() -> String {
    r#"{"type":"CONNECT"}"#.to_string()
}

/// `{"type":"CLOSE"}` — close a virtual channel.
pub fn close() -> String {
    r#"{"type":"CLOSE"}"#.to_string()
}

/// `{"type":"PING"}` — heartbeat request (sent on `NS_HEARTBEAT`).
pub fn ping() -> String {
    r#"{"type":"PING"}"#.to_string()
}

/// `{"type":"PONG"}` — heartbeat reply (sent on `NS_HEARTBEAT`).
pub fn pong() -> String {
    r#"{"type":"PONG"}"#.to_string()
}

/// `{"type":"LAUNCH","appId":<app_id>,"requestId":<req_id>}`
///
/// Launches the app identified by `app_id` (use [`DEFAULT_MEDIA_RECEIVER_APP_ID`]
/// for the standard media receiver).
pub fn launch(req_id: u32, app_id: &str) -> String {
    serde_json::json!({
        "type": "LAUNCH",
        "appId": app_id,
        "requestId": req_id,
    })
    .to_string()
}

/// `{"type":"GET_STATUS","requestId":<req_id>}`
///
/// Request the current receiver status (yields a RECEIVER_STATUS reply).
pub fn get_status(req_id: u32) -> String {
    serde_json::json!({
        "type": "GET_STATUS",
        "requestId": req_id,
    })
    .to_string()
}

/// LOAD payload for the Default Media Receiver.
///
/// ```json
/// {
///   "type": "LOAD",
///   "requestId": <req_id>,
///   "media": {
///     "contentId": <url>,
///     "contentType": <content_type>,
///     "streamType": "LIVE"
///   },
///   "autoplay": true
/// }
/// ```
pub fn load(req_id: u32, url: &str, content_type: &str) -> String {
    serde_json::json!({
        "type": "LOAD",
        "requestId": req_id,
        "media": {
            "contentId": url,
            "contentType": content_type,
            "streamType": "LIVE",
        },
        "autoplay": true,
    })
    .to_string()
}

/// Stop the current media session.
///
/// ```json
/// {"type":"STOP","requestId":<req_id>,"mediaSessionId":<media_session_id>}
/// ```
pub fn media_stop(req_id: u32, media_session_id: u32) -> String {
    serde_json::json!({
        "type": "STOP",
        "requestId": req_id,
        "mediaSessionId": media_session_id,
    })
    .to_string()
}

/// Stop the receiver application (sent on `NS_RECEIVER`).
///
/// ```json
/// {"type":"STOP","requestId":<req_id>}
/// ```
pub fn receiver_stop(req_id: u32) -> String {
    serde_json::json!({
        "type": "STOP",
        "requestId": req_id,
    })
    .to_string()
}

// ─── Status parse ─────────────────────────────────────────────────────────────

/// Extract the `transportId` of the launched app from a RECEIVER_STATUS payload.
///
/// The Chromecast sends a RECEIVER_STATUS message after a successful LAUNCH:
/// ```json
/// {
///   "type": "RECEIVER_STATUS",
///   "requestId": 1,
///   "status": {
///     "applications": [
///       {
///         "appId": "CC1AD845",
///         "transportId": "web-1",
///         ...
///       }
///     ]
///   }
/// }
/// ```
///
/// Returns `Some(transportId)` for the first application whose `appId` matches
/// `app_id`, or `None` if the payload is malformed or the app is absent.
pub fn parse_launched_transport_id(receiver_status_json: &str, app_id: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(receiver_status_json).ok()?;
    let apps = v.get("status")?.get("applications")?.as_array()?;
    for app in apps {
        if app.get("appId").and_then(|v| v.as_str()) == Some(app_id) {
            if let Some(tid) = app.get("transportId").and_then(|v| v.as_str()) {
                return Some(tid.to_string());
            }
        }
    }
    None
}

/// Extract the `"type"` field from a JSON payload.
///
/// Used to route incoming messages (e.g. `"PONG"`, `"RECEIVER_STATUS"`,
/// `"MEDIA_STATUS"`) without fully deserialising them.
///
/// Returns `None` if the payload is not valid JSON or lacks a string `"type"`.
pub fn message_type(payload_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(payload_json).ok()?;
    v.get("type")?.as_str().map(String::from)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── connect / close ───────────────────────────────────────────────────────

    #[test]
    fn connect_type_is_connect() {
        let v: serde_json::Value = serde_json::from_str(&connect()).unwrap();
        assert_eq!(v["type"], "CONNECT");
    }

    #[test]
    fn close_type_is_close() {
        let v: serde_json::Value = serde_json::from_str(&close()).unwrap();
        assert_eq!(v["type"], "CLOSE");
    }

    // ── ping / pong ───────────────────────────────────────────────────────────

    #[test]
    fn ping_type_is_ping() {
        let v: serde_json::Value = serde_json::from_str(&ping()).unwrap();
        assert_eq!(v["type"], "PING");
    }

    #[test]
    fn pong_type_is_pong() {
        let v: serde_json::Value = serde_json::from_str(&pong()).unwrap();
        assert_eq!(v["type"], "PONG");
    }

    // ── launch ────────────────────────────────────────────────────────────────

    #[test]
    fn launch_contains_type_app_id_and_request_id() {
        let json = launch(42, DEFAULT_MEDIA_RECEIVER_APP_ID);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "LAUNCH");
        assert_eq!(v["appId"], DEFAULT_MEDIA_RECEIVER_APP_ID);
        assert_eq!(v["requestId"], 42);
    }

    #[test]
    fn launch_request_id_zero() {
        let json = launch(0, "CC1AD845");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["requestId"], 0);
    }

    #[test]
    fn launch_custom_app_id() {
        let json = launch(1, "CUSTOM_APP");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["appId"], "CUSTOM_APP");
    }

    // ── get_status ────────────────────────────────────────────────────────────

    #[test]
    fn get_status_contains_type_and_request_id() {
        let json = get_status(7);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "GET_STATUS");
        assert_eq!(v["requestId"], 7);
    }

    // ── load ──────────────────────────────────────────────────────────────────

    #[test]
    fn load_contains_all_required_fields() {
        let url = "http://192.168.1.100:8080/stream.opus";
        let ct = "audio/ogg";
        let json = load(3, url, ct);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "LOAD");
        assert_eq!(v["requestId"], 3);
        assert!(v["autoplay"].as_bool().unwrap_or(false));
        let media = &v["media"];
        assert_eq!(media["contentId"], url);
        assert_eq!(media["contentType"], ct);
        assert_eq!(media["streamType"], "LIVE");
    }

    #[test]
    fn load_stream_type_is_live() {
        let v: serde_json::Value =
            serde_json::from_str(&load(1, "http://h/s", "audio/ogg")).unwrap();
        assert_eq!(v["media"]["streamType"], "LIVE");
    }

    // ── media_stop ────────────────────────────────────────────────────────────

    #[test]
    fn media_stop_contains_type_request_id_and_session() {
        let json = media_stop(5, 99);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "STOP");
        assert_eq!(v["requestId"], 5);
        assert_eq!(v["mediaSessionId"], 99);
    }

    // ── receiver_stop ─────────────────────────────────────────────────────────

    #[test]
    fn receiver_stop_contains_type_and_request_id() {
        let json = receiver_stop(10);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["type"], "STOP");
        assert_eq!(v["requestId"], 10);
        // receiver_stop does NOT include mediaSessionId (that's media_stop).
        assert!(v.get("mediaSessionId").is_none());
    }

    // ── parse_launched_transport_id ───────────────────────────────────────────

    /// Real-shape RECEIVER_STATUS as the Chromecast actually sends it.
    const RECEIVER_STATUS_SAMPLE: &str = r#"{
        "type": "RECEIVER_STATUS",
        "requestId": 1,
        "status": {
            "applications": [
                {
                    "appId": "CC1AD845",
                    "displayName": "Default Media Receiver",
                    "isIdleScreen": false,
                    "launchedFromCloud": false,
                    "namespaces": [
                        { "name": "urn:x-cast:com.google.cast.media" }
                    ],
                    "sessionId": "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
                    "statusText": "",
                    "transportId": "web-1"
                }
            ],
            "isActiveInput": true,
            "isStandBy": false,
            "volume": {
                "controlType": "master",
                "level": 1.0,
                "muted": false,
                "stepInterval": 0.05
            }
        }
    }"#;

    #[test]
    fn parse_launched_transport_id_finds_cc1ad845() {
        let tid = parse_launched_transport_id(RECEIVER_STATUS_SAMPLE, "CC1AD845");
        assert_eq!(tid, Some("web-1".to_string()));
    }

    #[test]
    fn parse_launched_transport_id_wrong_app_id_returns_none() {
        let tid = parse_launched_transport_id(RECEIVER_STATUS_SAMPLE, "NONEXISTENT");
        assert_eq!(tid, None);
    }

    #[test]
    fn parse_launched_transport_id_empty_json_returns_none() {
        assert_eq!(parse_launched_transport_id("{}", "CC1AD845"), None);
    }

    #[test]
    fn parse_launched_transport_id_malformed_json_returns_none() {
        assert_eq!(
            parse_launched_transport_id("not json {{{", "CC1AD845"),
            None
        );
    }

    #[test]
    fn parse_launched_transport_id_missing_status_returns_none() {
        let json = r#"{"type":"RECEIVER_STATUS","requestId":1}"#;
        assert_eq!(parse_launched_transport_id(json, "CC1AD845"), None);
    }

    #[test]
    fn parse_launched_transport_id_empty_applications_returns_none() {
        let json = r#"{"type":"RECEIVER_STATUS","status":{"applications":[]}}"#;
        assert_eq!(parse_launched_transport_id(json, "CC1AD845"), None);
    }

    #[test]
    fn parse_launched_transport_id_multiple_apps_finds_correct_one() {
        let json = r#"{
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": [
                    {"appId": "OTHER_APP", "transportId": "web-0"},
                    {"appId": "CC1AD845",  "transportId": "web-1"}
                ]
            }
        }"#;
        let tid = parse_launched_transport_id(json, "CC1AD845");
        assert_eq!(tid, Some("web-1".to_string()));
    }

    /// Real Chromecasts include Backdrop / idle entries (e.g. `E8C28D3C`) that
    /// may appear before the target app and lack an `appId` field (or have it as
    /// `null`).  The old `?`-in-loop code returned `None` as soon as it hit such
    /// an entry; the fixed code skips them and continues searching.
    #[test]
    fn parse_launched_transport_id_skips_no_app_id_entry_before_target() {
        let json = r#"{
            "type": "RECEIVER_STATUS",
            "status": {
                "applications": [
                    {"displayName": "Backdrop", "transportId": "bg-0"},
                    {"appId": null, "displayName": "Idle", "transportId": "idle-0"},
                    {"appId": "CC1AD845", "transportId": "web-1"}
                ]
            }
        }"#;
        let tid = parse_launched_transport_id(json, "CC1AD845");
        assert_eq!(
            tid,
            Some("web-1".to_string()),
            "should skip the no-appId entries and find CC1AD845"
        );
    }

    #[test]
    fn parse_launched_transport_id_uses_constant() {
        let tid =
            parse_launched_transport_id(RECEIVER_STATUS_SAMPLE, DEFAULT_MEDIA_RECEIVER_APP_ID);
        assert_eq!(tid, Some("web-1".to_string()));
    }

    // ── message_type ──────────────────────────────────────────────────────────

    #[test]
    fn message_type_extracts_receiver_status() {
        assert_eq!(
            message_type(RECEIVER_STATUS_SAMPLE),
            Some("RECEIVER_STATUS".to_string())
        );
    }

    #[test]
    fn message_type_extracts_pong() {
        assert_eq!(message_type(&pong()), Some("PONG".to_string()));
    }

    #[test]
    fn message_type_extracts_ping() {
        assert_eq!(message_type(&ping()), Some("PING".to_string()));
    }

    #[test]
    fn message_type_extracts_media_status() {
        let json = r#"{"type":"MEDIA_STATUS","status":[]}"#;
        assert_eq!(message_type(json), Some("MEDIA_STATUS".to_string()));
    }

    #[test]
    fn message_type_missing_type_field_returns_none() {
        assert_eq!(message_type(r#"{"requestId":1}"#), None);
    }

    #[test]
    fn message_type_malformed_json_returns_none() {
        assert_eq!(message_type("not json"), None);
    }

    #[test]
    fn message_type_numeric_type_returns_none() {
        // type field must be a string, not a number.
        assert_eq!(message_type(r#"{"type":42}"#), None);
    }

    // ── namespace constants ───────────────────────────────────────────────────

    #[test]
    fn namespace_constants_have_correct_values() {
        assert!(NS_CONNECTION.contains("connection"));
        assert!(NS_HEARTBEAT.contains("heartbeat"));
        assert!(NS_RECEIVER.contains("receiver"));
        assert!(NS_MEDIA.contains("media"));
        assert_eq!(DEFAULT_MEDIA_RECEIVER_APP_ID, "CC1AD845");
    }
}
