//! Chromecast CASTV2 protocol implementation.
//!
//! - [`proto`] — `CastMessage` protobuf framing (4-byte BE length prefix).
//! - [`messages`] — JSON payload builders + RECEIVER_STATUS parse.
//! - [`client`] — TLS session, URL helpers, and `CastHandle`/`start_cast`.

pub mod client;
pub mod messages;
pub mod proto;
