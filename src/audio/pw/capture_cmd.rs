//! Build the PCM capture command argv (parec or pw-cat). AUD-034/036/037, UB-1.
use crate::audio::frame::{CHANNELS, SAMPLE_RATE};

/// Where to capture PCM from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureSource {
    /// The null sink's monitor (EQ-processed audio). For parec the `.monitor`
    /// suffix is appended; for pw-cat the bare sink node name is targeted.
    NullSinkMonitor(String),
    /// A Bluetooth source node directly (no EQ — last-resort fallback).
    BluetoothDirect(String),
    /// The default monitor/source.
    Default,
}

/// Which capture tool to invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTool {
    Parec,
    PwCat,
}

/// A ready-to-spawn capture command (program + argv).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// Build the capture command. `parec` targets PulseAudio source names (the null
/// sink monitor is `<sink>.monitor`, AUD-034) and takes an explicit
/// `--latency-msec` derived from the live quantum (UB-1); `pw-cat` targets the
/// PipeWire node name and has no latency flag.
pub fn build_capture_command(
    tool: CaptureTool,
    source: &CaptureSource,
    latency_ms: u32,
) -> CaptureCommand {
    match tool {
        CaptureTool::Parec => {
            let device = match source {
                CaptureSource::NullSinkMonitor(name) => format!("{name}.monitor"),
                CaptureSource::BluetoothDirect(name) => name.clone(),
                CaptureSource::Default => "@DEFAULT_MONITOR@".to_string(),
            };
            CaptureCommand {
                program: "parec",
                args: vec![
                    "--raw".to_string(),
                    "--format=float32".to_string(),
                    format!("--channels={CHANNELS}"),
                    format!("--rate={SAMPLE_RATE}"),
                    format!("--device={device}"),
                    format!("--latency-msec={latency_ms}"),
                ],
            }
        }
        CaptureTool::PwCat => {
            let mut args = vec![
                "--format".to_string(),
                "f32".to_string(),
                "--channels".to_string(),
                CHANNELS.to_string(),
                "--rate".to_string(),
                SAMPLE_RATE.to_string(),
                "-r".to_string(),
            ];
            match source {
                CaptureSource::NullSinkMonitor(name) | CaptureSource::BluetoothDirect(name) => {
                    args.push("--target".to_string());
                    args.push(name.clone());
                }
                CaptureSource::Default => {}
            }
            args.push("-".to_string());
            CaptureCommand {
                program: "pw-cat",
                args,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parec_targets_monitor_with_derived_latency() {
        let cmd = build_capture_command(
            CaptureTool::Parec,
            &CaptureSource::NullSinkMonitor("soundsync-capture".into()),
            50,
        );
        assert_eq!(cmd.program, "parec");
        // --raw is load-bearing: without it parec emits a WAV header and corrupts the stream.
        assert!(cmd.args.contains(&"--raw".to_string()));
        assert!(cmd
            .args
            .contains(&"--device=soundsync-capture.monitor".to_string()));
        assert!(cmd.args.contains(&"--latency-msec=50".to_string()));
        assert!(cmd.args.contains(&"--format=float32".to_string()));
        assert!(cmd.args.contains(&"--channels=2".to_string()));
        assert!(cmd.args.contains(&"--rate=48000".to_string()));
    }

    #[test]
    fn parec_default_uses_default_monitor() {
        let cmd = build_capture_command(CaptureTool::Parec, &CaptureSource::Default, 50);
        assert!(cmd.args.contains(&"--device=@DEFAULT_MONITOR@".to_string()));
    }

    #[test]
    fn parec_bluetooth_direct_uses_raw_name() {
        let cmd = build_capture_command(
            CaptureTool::Parec,
            &CaptureSource::BluetoothDirect("bluez_input.AA_BB".into()),
            86,
        );
        assert!(cmd.args.contains(&"--device=bluez_input.AA_BB".to_string()));
        assert!(cmd.args.contains(&"--latency-msec=86".to_string()));
    }

    #[test]
    fn pwcat_targets_node_name_no_latency_flag() {
        let cmd = build_capture_command(
            CaptureTool::PwCat,
            &CaptureSource::NullSinkMonitor("soundsync-capture".into()),
            50,
        );
        assert_eq!(cmd.program, "pw-cat");
        // -r is load-bearing: without it pw-cat plays back instead of capturing.
        assert!(cmd.args.contains(&"-r".to_string()));
        // pw-cat targets the sink node itself (no .monitor suffix) and records it.
        let i = cmd.args.iter().position(|a| a == "--target").unwrap();
        assert_eq!(cmd.args[i + 1], "soundsync-capture");
        assert_eq!(cmd.args.last().unwrap(), "-");
        assert!(!cmd.args.iter().any(|a| a.contains("latency")));
    }

    #[test]
    fn pwcat_default_has_no_target() {
        let cmd = build_capture_command(CaptureTool::PwCat, &CaptureSource::Default, 50);
        assert!(!cmd.args.iter().any(|a| a == "--target"));
        assert_eq!(cmd.args.last().unwrap(), "-");
    }
}
