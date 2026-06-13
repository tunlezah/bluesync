//! Multi-output subsystem (multi-output-design.md).
//!
//! OUT-1: model + discovery parsers + control surface skeleton.
//! OUT-2/3/4: real audio routing (loopback / RAOP / Chromecast) added later.

pub mod controller;
pub mod discovery;
pub mod loopback;

pub use loopback::{build_loopback_args, parse_loopback_module_id, CAPTURE_MONITOR};

use serde::Serialize;

/// Which output technology this device uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    Soundcard,
    Airplay,
    Chromecast,
}

impl OutputKind {
    /// Snake-case wire string (matches serde).
    pub fn as_str(&self) -> &'static str {
        match self {
            OutputKind::Soundcard => "soundcard",
            OutputKind::Airplay => "airplay",
            OutputKind::Chromecast => "chromecast",
        }
    }
}

/// A discovered output target.
///
/// `id` identifies the device unambiguously:
/// - soundcard/airplay: the PipeWire sink name (e.g. `alsa_output.usb-FiiO_BTR3…`).
/// - chromecast: the `id=` TXT field from mDNS (e.g. `d122a7025b65…`).
///
/// OUT-3 refines the AirPlay id to the actual PW sink name once
/// `module-raop-discover` maps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutputDevice {
    pub kind: OutputKind,
    /// Stable opaque identifier (sink name or cast id).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// mDNS host (`.local` address) or `None` for local sound cards.
    pub addr: Option<String>,
    /// Service port (`None` for local sound cards).
    pub port: Option<u16>,
}

/// Aggregated discovery results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct AvailableOutputs {
    pub soundcard: Vec<OutputDevice>,
    pub airplay: Vec<OutputDevice>,
    pub chromecast: Vec<OutputDevice>,
}

/// Commands the `OutputController` accepts.
#[derive(Debug)]
pub enum OutputCommand {
    /// Select a specific output by kind + id.
    Select { kind: OutputKind, id: String },
    /// Clear the active output (revert to browser-only).
    None,
}
