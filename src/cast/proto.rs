//! CASTV2 `CastMessage` protobuf framing.
//!
//! Chromecast uses length-prefixed protobuf frames over TLS:8009:
//! - 4-byte big-endian length prefix (the byte length of the protobuf payload)
//! - followed by the serialised `CastMessage`
//!
//! `CastMessage` is defined inline via `#[derive(prost::Message)]` — no
//! build.rs or `.proto` files needed.

/// Protocol version enum values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ProtocolVersion {
    Castv210 = 0,
}

/// Payload encoding enum values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum PayloadType {
    String = 0,
    Binary = 1,
}

/// CASTV2 wire message.
///
/// All fields map directly to the Chromecast protobuf definition (field
/// numbers are fixed by the protocol and must not be changed).
#[derive(Clone, PartialEq, prost::Message)]
pub struct CastMessage {
    /// Must be `ProtocolVersion::Castv210 as i32` (= 0) for all current cast traffic.
    /// `optional` (not a bare enum) so prost ENCODES it even when 0 — the real
    /// Chromecast proto marks this `required` and drops messages that omit it.
    #[prost(enumeration = "ProtocolVersion", optional, tag = "1")]
    pub protocol_version: Option<i32>,

    /// Logical sender id, e.g. `"sender-0"`.
    #[prost(string, tag = "2")]
    pub source_id: String,

    /// Logical destination id, e.g. `"receiver-0"` or a transportId.
    #[prost(string, tag = "3")]
    pub destination_id: String,

    /// Namespace URI, e.g. `"urn:x-cast:com.google.cast.tp.connection"`.
    #[prost(string, tag = "4")]
    pub namespace: String,

    /// `PayloadType::String` (0) for JSON payloads, `Binary` (1) for binary.
    /// `optional` so prost encodes it even for `String`(0) — `required` on the wire.
    #[prost(enumeration = "PayloadType", optional, tag = "5")]
    pub payload_type: Option<i32>,

    /// UTF-8 JSON payload (present when `payload_type` == String).
    #[prost(string, optional, tag = "6")]
    pub payload_utf8: Option<String>,

    /// Binary payload (present when `payload_type` == Binary).
    #[prost(bytes = "vec", optional, tag = "7")]
    pub payload_binary: Option<Vec<u8>>,
}

/// Encode a `CastMessage` into a length-prefixed frame ready to write to the
/// TLS socket.
///
/// Frame layout: `[ 4-byte big-endian length ][ protobuf bytes ]`
pub fn encode_frame(msg: &CastMessage) -> Vec<u8> {
    use prost::Message as _;
    let payload = msg.encode_to_vec();
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// Try to decode a `CastMessage` from the front of `buf`.
///
/// Returns `Some((msg, bytes_consumed))` when `buf` contains a complete frame,
/// or `None` when more bytes are needed (the buffer is unchanged — callers
/// should keep accumulating bytes and retry).
///
/// `bytes_consumed` = 4 (length prefix) + the protobuf payload length.
pub fn decode_frame(buf: &[u8]) -> Option<(CastMessage, usize)> {
    use prost::Message as _;
    if buf.len() < 4 {
        return None;
    }
    let msg_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let total = 4 + msg_len;
    if buf.len() < total {
        return None;
    }
    let msg = CastMessage::decode(&buf[4..total]).ok()?;
    Some((msg, total))
}

/// Build a string-payload `CastMessage` with the canonical sender id.
///
/// - `source_id` is always `"sender-0"`.
/// - `payload_type` is always `PayloadType::String`.
/// - `payload_utf8` is set to the provided JSON string.
pub fn string_message(namespace: &str, destination_id: &str, payload_json: &str) -> CastMessage {
    CastMessage {
        protocol_version: Some(ProtocolVersion::Castv210 as i32),
        source_id: "sender-0".to_string(),
        destination_id: destination_id.to_string(),
        namespace: namespace.to_string(),
        payload_type: Some(PayloadType::String as i32),
        payload_utf8: Some(payload_json.to_string()),
        payload_binary: None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_msg() -> CastMessage {
        string_message(
            "urn:x-cast:com.google.cast.tp.connection",
            "receiver-0",
            r#"{"type":"CONNECT"}"#,
        )
    }

    // ── encode / decode roundtrip ─────────────────────────────────────────────

    #[test]
    fn roundtrip_string_message() {
        let msg = sample_msg();
        let frame = encode_frame(&msg);
        let (decoded, consumed) = decode_frame(&frame).expect("should decode");
        assert_eq!(consumed, frame.len(), "all bytes consumed");
        assert_eq!(decoded.source_id, "sender-0");
        assert_eq!(decoded.destination_id, "receiver-0");
        assert_eq!(
            decoded.namespace,
            "urn:x-cast:com.google.cast.tp.connection"
        );
        assert_eq!(
            decoded.payload_utf8.as_deref(),
            Some(r#"{"type":"CONNECT"}"#)
        );
        assert_eq!(decoded.payload_type, Some(PayloadType::String as i32));
        assert_eq!(
            decoded.protocol_version,
            Some(ProtocolVersion::Castv210 as i32)
        );
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let msg = CastMessage {
            protocol_version: Some(ProtocolVersion::Castv210 as i32),
            source_id: "sender-0".to_string(),
            destination_id: "transport-abc123".to_string(),
            namespace: "urn:x-cast:com.google.cast.media".to_string(),
            payload_type: Some(PayloadType::String as i32),
            payload_utf8: Some(r#"{"type":"LOAD","requestId":2}"#.to_string()),
            payload_binary: None,
        };
        let frame = encode_frame(&msg);
        let (decoded, _) = decode_frame(&frame).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_binary_payload() {
        let msg = CastMessage {
            protocol_version: Some(ProtocolVersion::Castv210 as i32),
            source_id: "sender-0".to_string(),
            destination_id: "receiver-0".to_string(),
            namespace: "urn:x-cast:com.google.cast.tp.heartbeat".to_string(),
            payload_type: Some(PayloadType::Binary as i32),
            payload_utf8: None,
            payload_binary: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        };
        let frame = encode_frame(&msg);
        let (decoded, consumed) = decode_frame(&frame).unwrap();
        assert_eq!(consumed, frame.len());
        assert_eq!(decoded.payload_binary, Some(vec![0xDE, 0xAD, 0xBE, 0xEF]));
        assert_eq!(decoded.payload_type, Some(PayloadType::Binary as i32));
    }

    // ── 4-byte length prefix correctness ─────────────────────────────────────

    #[test]
    fn encode_frame_length_prefix_is_correct() {
        let msg = sample_msg();
        let frame = encode_frame(&msg);
        // First 4 bytes are big-endian length of the protobuf payload.
        let prefix = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        assert_eq!(
            prefix,
            frame.len() - 4,
            "length prefix must equal payload size"
        );
    }

    #[test]
    fn encode_frame_starts_with_4_byte_prefix() {
        let frame = encode_frame(&sample_msg());
        assert!(
            frame.len() > 4,
            "frame must be longer than the 4-byte prefix"
        );
    }

    // ── decode_frame with insufficient bytes ──────────────────────────────────

    #[test]
    fn decode_frame_too_short_for_length_prefix_returns_none() {
        assert_eq!(decode_frame(&[]), None);
        assert_eq!(decode_frame(&[0]), None);
        assert_eq!(decode_frame(&[0, 0]), None);
        assert_eq!(decode_frame(&[0, 0, 0]), None);
    }

    #[test]
    fn decode_frame_has_prefix_but_incomplete_payload_returns_none() {
        let msg = sample_msg();
        let frame = encode_frame(&msg);
        // Provide prefix + only half the payload.
        let partial = &frame[..4 + (frame.len() - 4) / 2];
        assert_eq!(decode_frame(partial), None);
    }

    #[test]
    fn decode_frame_exactly_one_frame_with_trailing_bytes() {
        let msg = sample_msg();
        let frame = encode_frame(&msg);
        // Append some garbage bytes after the frame.
        let mut extended = frame.clone();
        extended.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        let (decoded, consumed) = decode_frame(&extended).unwrap();
        assert_eq!(consumed, frame.len(), "only the first frame is consumed");
        assert_eq!(decoded.source_id, "sender-0");
    }

    #[test]
    fn decode_frame_reports_consumed_bytes() {
        let msg = sample_msg();
        let frame = encode_frame(&msg);
        let (_, consumed) = decode_frame(&frame).unwrap();
        assert_eq!(consumed, frame.len());
    }

    // ── string_message helper ─────────────────────────────────────────────────

    #[test]
    fn string_message_sets_sender_id() {
        let msg = string_message("ns", "dest", "{}");
        assert_eq!(msg.source_id, "sender-0");
    }

    #[test]
    fn string_message_sets_payload_type_string() {
        let msg = string_message("ns", "dest", "{}");
        assert_eq!(msg.payload_type, Some(PayloadType::String as i32));
    }

    #[test]
    fn string_message_no_binary_payload() {
        let msg = string_message("ns", "dest", "{}");
        assert!(msg.payload_binary.is_none());
    }

    #[test]
    fn string_message_sets_namespace_and_dest() {
        let msg = string_message("urn:x-cast:com.google.cast.receiver", "receiver-0", "{}");
        assert_eq!(msg.namespace, "urn:x-cast:com.google.cast.receiver");
        assert_eq!(msg.destination_id, "receiver-0");
    }

    #[test]
    fn string_message_protocol_version_is_castv2_1_0() {
        let msg = string_message("ns", "dest", "{}");
        assert_eq!(msg.protocol_version, Some(ProtocolVersion::Castv210 as i32));
    }

    // ── Chromecast-compat regression guard ───────────────────────────────────
    //
    // `protocol_version` (field 1) and `payload_type` (field 5) are both
    // zero-valued for the most common message kind (CASTV2_1_0 + String).
    // Prost normally omits fields whose wire value equals the default (0 / "").
    // The fields are declared `optional` so prost is forced to encode them even
    // when they are zero.  The real Chromecast firmware requires these fields to
    // be present on the wire and silently drops messages that omit them — which
    // is exactly the bug this fix addresses (LIVE-VERIFIED).
    //
    // This test encodes a `string_message` (both zero-valued fields), strips the
    // 4-byte length prefix, decodes the raw protobuf, and asserts that both
    // fields decode as `Some(0)` — proving they were present in the wire bytes.
    #[test]
    fn wire_encoding_includes_zero_valued_protocol_version_and_payload_type() {
        use prost::Message as _;

        let msg = string_message(
            "urn:x-cast:com.google.cast.tp.connection",
            "receiver-0",
            r#"{"type":"CONNECT"}"#,
        );
        let frame = encode_frame(&msg);
        // Skip the 4-byte big-endian length prefix to get the raw protobuf bytes.
        let proto_bytes = &frame[4..];
        let decoded = CastMessage::decode(proto_bytes).expect("must decode");

        // Both fields are zero-valued but must be present (Some) on the wire.
        assert_eq!(
            decoded.protocol_version,
            Some(ProtocolVersion::Castv210 as i32),
            "protocol_version must be encoded even though its value is 0 (Chromecast-compat)"
        );
        assert_eq!(
            decoded.payload_type,
            Some(PayloadType::String as i32),
            "payload_type must be encoded even though its value is 0 (Chromecast-compat)"
        );
    }
}
