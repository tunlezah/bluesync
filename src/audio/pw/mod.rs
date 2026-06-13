//! PipeWire/PulseAudio control plane (run-to-completion pactl/wpctl operations).
pub mod capture_cmd;
pub mod events;
pub mod filter_ctl;
pub mod null_sink;
pub mod parse;
pub mod quantum;
pub mod routing;
