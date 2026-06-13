//! AUD-010: detect the `bluez_input.*` PipeWire node — the real "audio is
//! flowing" signal for a connected A2DP source. BlueZ "connected" is not enough;
//! WirePlumber must acquire the transport and create the node first. Detected via
//! `pactl list short sources` (the legacy signal), over the `CommandRunner` seam.
use crate::sys::command::CommandRunner;
use std::collections::BTreeSet;

/// PipeWire node-name prefix for an A2DP source captured by BlueZ/WirePlumber.
pub const BLUEZ_INPUT_PREFIX: &str = "bluez_input.";

/// Extract the device MAC addresses of all `bluez_input.<MAC>.*` nodes in a
/// `pactl list short sources` output. The node name encodes the MAC with
/// underscores (`AA_BB_..`); we render it canonical (`AA:BB:..`, AUD-012). Order
/// follows the input lines.
pub fn bluez_input_addresses(sources_output: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in sources_output.lines() {
        for token in line.split_whitespace() {
            if let Some(rest) = token.strip_prefix(BLUEZ_INPUT_PREFIX) {
                // `<AA_BB_..>` up to the next `.` (e.g. `.a2dp-source`), if any.
                let mac_us = rest.split('.').next().unwrap_or(rest);
                let mac = mac_us.replace('_', ":");
                if !mac.is_empty() {
                    out.push(mac);
                }
            }
        }
    }
    out
}

/// Run `pactl list short sources` and return the set of device MACs that
/// currently have a `bluez_input.*` node (the audio-active set; AUD-010).
pub async fn probe_bluez_input_addresses<R: CommandRunner>(
    runner: &R,
) -> Result<BTreeSet<String>, String> {
    let out = runner
        .run("pactl", &["list", "short", "sources"])
        .map_err(|e| format!("failed to run pactl: {e}"))?;
    if !out.success() {
        return Err(format!(
            "pactl list short sources failed: {}",
            out.stderr.trim()
        ));
    }
    Ok(bluez_input_addresses(&out.stdout).into_iter().collect())
}

/// Whether `pactl list short sources` output contains a `bluez_input.*` node.
pub fn has_bluez_input_node(sources_output: &str) -> bool {
    sources_output
        .lines()
        .any(|line| line.contains(BLUEZ_INPUT_PREFIX))
}

/// Run `pactl list short sources` and report whether a `bluez_input.*` node
/// is present (the AudioActive gate).
pub async fn probe_bluez_input<R: CommandRunner>(runner: &R) -> Result<bool, String> {
    let out = runner
        .run("pactl", &["list", "short", "sources"])
        .map_err(|e| format!("failed to run pactl: {e}"))?;
    if !out.success() {
        return Err(format!(
            "pactl list short sources failed: {}",
            out.stderr.trim()
        ));
    }
    Ok(has_bluez_input_node(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;

    fn out(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn detects_bluez_input_source() {
        let sources = "42\tbluez_input.AA_BB_CC_DD_EE_FF.a2dp-source\tPipeWire\ts16le 2ch 48000Hz\tRUNNING\n\
                       43\talsa_input.platform-soc.analog-stereo\tPipeWire\ts16le 2ch 44100Hz\tSUSPENDED";
        assert!(has_bluez_input_node(sources));
    }

    #[test]
    fn no_bluez_input_when_only_other_sources() {
        let sources =
            "43\talsa_input.platform-soc.analog-stereo\tPipeWire\ts16le 2ch 44100Hz\tSUSPENDED\n\
                       44\tsoundsync-capture.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE";
        assert!(!has_bluez_input_node(sources));
    }

    #[test]
    fn empty_output_has_no_node() {
        assert!(!has_bluez_input_node(""));
    }

    #[tokio::test]
    async fn probe_runs_pactl_and_reports_present() {
        let runner = FakeCommandRunner::new().on(
            "pactl",
            &["list", "short", "sources"],
            out("50\tbluez_input.11_22_33_44_55_66.a2dp-source\tPipeWire\ts16le\tRUNNING"),
        );
        assert!(probe_bluez_input(&runner).await.unwrap());
        let (prog, args) = &runner.calls()[0];
        assert_eq!(prog, "pactl");
        assert_eq!(
            args,
            &vec![
                "list".to_string(),
                "short".to_string(),
                "sources".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn probe_reports_absent() {
        let runner = FakeCommandRunner::new().on(
            "pactl",
            &["list", "short", "sources"],
            out(
                "43\talsa_input.platform-soc.analog-stereo\tPipeWire\ts16le 2ch 44100Hz\tSUSPENDED",
            ),
        );
        assert!(!probe_bluez_input(&runner).await.unwrap());
        let (prog, args) = &runner.calls()[0];
        assert_eq!(prog, "pactl");
        assert_eq!(
            args,
            &vec![
                "list".to_string(),
                "short".to_string(),
                "sources".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn probe_propagates_command_error() {
        let runner = FakeCommandRunner::new().with(
            "pactl",
            CommandOutput {
                status: 1,
                stdout: String::new(),
                stderr: "connection refused".into(),
            },
        );
        assert!(probe_bluez_input(&runner).await.is_err());
    }

    #[test]
    fn extracts_mac_from_bluez_input_node() {
        let sources = "50\tbluez_input.AA_BB_CC_DD_EE_FF.a2dp-source\tPipeWire\ts16le\tRUNNING\n\
                       51\talsa_input.platform.analog-stereo\tPipeWire\ts16le\tIDLE";
        assert_eq!(
            bluez_input_addresses(sources),
            vec!["AA:BB:CC:DD:EE:FF".to_string()]
        );
    }

    #[test]
    fn extracts_multiple_and_ignores_non_bluez() {
        let sources = "1\tbluez_input.11_22_33_44_55_66.a2dp-source\tPipeWire\ts16le\tRUNNING\n\
                       2\tsoundsync-capture.monitor\tPipeWire\tfloat32le\tIDLE\n\
                       3\tbluez_input.AA_BB_CC_DD_EE_FF\tPipeWire\ts16le\tRUNNING";
        assert_eq!(
            bluez_input_addresses(sources),
            vec![
                "11:22:33:44:55:66".to_string(),
                "AA:BB:CC:DD:EE:FF".to_string()
            ]
        );
    }

    #[test]
    fn no_addresses_when_none_present() {
        assert!(bluez_input_addresses("3\talsa_input.x\tPipeWire\ts16le\tIDLE").is_empty());
    }

    #[tokio::test]
    async fn probe_addresses_runs_pactl_and_parses() {
        let runner = FakeCommandRunner::new().on(
            "pactl",
            &["list", "short", "sources"],
            out("9\tbluez_input.01_23_45_67_89_AB.a2dp-source\tPipeWire\ts16le\tRUNNING"),
        );
        let addrs = probe_bluez_input_addresses(&runner).await.unwrap();
        assert!(addrs.contains("01:23:45:67:89:AB"));
        assert_eq!(addrs.len(), 1);
    }
}
