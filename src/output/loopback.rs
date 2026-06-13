//! PipeWire module-loopback command builders for local sound-card output.
//!
//! OUT-2: builds the `pactl load-module module-loopback` argv and parses the
//! returned module id.  These functions are pure (no I/O) so they are fully
//! unit-tested without spawning processes.
//!
//! Architecture: `soundsync-capture.monitor` is the multi-consumer monitor
//! source for the capture null-sink.  Loading `module-loopback` with
//! `source=soundsync-capture.monitor sink=<target>` creates a live copy of the
//! capture audio to the chosen hardware sink.  The `*_dont_move=true` flags
//! prevent PipeWire from silently re-routing the loopback away from the target
//! sink or source (e.g. when a higher-priority default changes).
//!
//! OUT-3 reuses `build_loopback_args` for the RAOP sink (AirPlay).

/// The PipeWire monitor source that carries the capture audio to any number
/// of downstream loopback consumers.
pub const CAPTURE_MONITOR: &str = "soundsync-capture.monitor";

/// Build the `pactl load-module` argument list for a loopback from
/// [`CAPTURE_MONITOR`] to `sink`.
///
/// Returns the argv slice passed to `pactl` (i.e. everything *after* the
/// `pactl` binary name):
/// ```text
/// load-module module-loopback source=soundsync-capture.monitor sink=<sink>
///     sink_dont_move=true source_dont_move=true
/// ```
pub fn build_loopback_args(sink: &str) -> Vec<String> {
    vec![
        "load-module".to_string(),
        "module-loopback".to_string(),
        format!("source={CAPTURE_MONITOR}"),
        format!("sink={sink}"),
        "sink_dont_move=true".to_string(),
        "source_dont_move=true".to_string(),
    ]
}

/// Parse the output of `pactl load-module` into a module index.
///
/// On success `pactl load-module` prints the new module index as a bare
/// decimal number followed by a newline, e.g. `"536870913\n"`.  Returns
/// `None` if the string cannot be trimmed to a valid `u32`.
pub fn parse_loopback_module_id(load_output: &str) -> Option<u32> {
    load_output.trim().parse::<u32>().ok()
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_loopback_args_exact_argv() {
        let sink = "alsa_output.usb-FiiO_BTR3-00.analog-stereo";
        let args = build_loopback_args(sink);
        assert_eq!(
            args,
            vec![
                "load-module".to_string(),
                "module-loopback".to_string(),
                "source=soundsync-capture.monitor".to_string(),
                format!("sink={sink}"),
                "sink_dont_move=true".to_string(),
                "source_dont_move=true".to_string(),
            ]
        );
    }

    #[test]
    fn build_loopback_args_source_is_capture_monitor() {
        let args = build_loopback_args("any-sink");
        assert_eq!(args[2], format!("source={CAPTURE_MONITOR}"));
    }

    #[test]
    fn parse_loopback_module_id_bare_number() {
        assert_eq!(parse_loopback_module_id("536870913\n"), Some(536870913));
    }

    #[test]
    fn parse_loopback_module_id_no_trailing_newline() {
        assert_eq!(parse_loopback_module_id("42"), Some(42));
    }

    #[test]
    fn parse_loopback_module_id_with_whitespace() {
        assert_eq!(parse_loopback_module_id("  100\n"), Some(100));
    }

    #[test]
    fn parse_loopback_module_id_garbage_returns_none() {
        assert_eq!(parse_loopback_module_id("failure"), None);
        assert_eq!(parse_loopback_module_id(""), None);
        assert_eq!(parse_loopback_module_id("Module #3"), None);
    }

    #[test]
    fn parse_loopback_module_id_negative_returns_none() {
        // i32 negative values must not parse as u32.
        assert_eq!(parse_loopback_module_id("-1"), None);
    }

    #[test]
    fn parse_loopback_module_id_overflow_returns_none() {
        // 2^32 overflows u32.
        assert_eq!(parse_loopback_module_id("4294967296"), None);
    }
}
