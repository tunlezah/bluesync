//! Adapter configuration that BlueZ/bluer can't do, via `hciconfig` (AUD-002/003).
use crate::sys::command::CommandRunner;

/// Audio/Video major, HiFi minor, Rendering+Audio service bits. A2DP *source*
/// devices filter inquiry candidates by CoD and skip non-sink-looking adapters,
/// so this must be forced (BlueZ 5 ignores `Class=` in main.conf; bluer has no
/// setter). AUD-002.
pub const ADAPTER_COD: &str = "0x240414";

/// Force the adapter's Class-of-Device via `hciconfig <hci> class 0x240414`.
pub fn set_class_of_device<R: CommandRunner>(runner: &R, hci: &str) -> Result<(), String> {
    run_hciconfig(runner, &[hci, "class", ADAPTER_COD])
}

/// Set the adapter's HCI-level name (belt-and-braces with the BlueZ alias —
/// some source firmwares read the raw HCI name). AUD-003.
pub fn set_hci_name<R: CommandRunner>(runner: &R, hci: &str, name: &str) -> Result<(), String> {
    run_hciconfig(runner, &[hci, "name", name])
}

fn run_hciconfig<R: CommandRunner>(runner: &R, args: &[&str]) -> Result<(), String> {
    let out = runner
        .run("hciconfig", args)
        .map_err(|e| format!("failed to run hciconfig: {e}"))?;
    if out.success() {
        Ok(())
    } else {
        Err(format!("hciconfig {args:?} failed: {}", out.stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;

    fn ok() -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn sets_audio_class_of_device() {
        let runner = FakeCommandRunner::new().on("hciconfig", &["class"], ok());
        set_class_of_device(&runner, "hci0").unwrap();
        let (prog, args) = &runner.calls()[0];
        assert_eq!(prog, "hciconfig");
        assert_eq!(
            args,
            &vec![
                "hci0".to_string(),
                "class".to_string(),
                ADAPTER_COD.to_string()
            ]
        );
    }

    #[test]
    fn sets_hci_name() {
        let runner = FakeCommandRunner::new().on("hciconfig", &["name"], ok());
        set_hci_name(&runner, "hci0", "SoundSync").unwrap();
        assert_eq!(
            runner.calls()[0].1,
            vec![
                "hci0".to_string(),
                "name".to_string(),
                "SoundSync".to_string()
            ]
        );
    }

    #[test]
    fn class_of_device_decodes_as_audio_video_rendering() {
        let cod = u32::from_str_radix(ADAPTER_COD.trim_start_matches("0x"), 16).unwrap();
        // Major Device Class (bits 12..=8) == Audio/Video (0b00100).
        assert_eq!(
            (cod >> 8) & 0x1f,
            0b00100,
            "major device class must be Audio/Video"
        );
        // Audio service class bit (bit 21) set.
        assert_ne!(cod & (1 << 21), 0, "Audio service class bit must be set");
    }

    #[test]
    fn spawn_error_returns_failed_to_run_hciconfig() {
        let runner = FakeCommandRunner::new().with_error("hciconfig");
        let err = set_class_of_device(&runner, "hci0").unwrap_err();
        assert!(
            err.contains("failed to run hciconfig"),
            "expected 'failed to run hciconfig' in error, got: {err}"
        );
    }

    #[test]
    fn errors_propagate() {
        let runner = FakeCommandRunner::new().on(
            "hciconfig",
            &["class"],
            CommandOutput {
                status: 1,
                stdout: String::new(),
                stderr: "no such device".into(),
            },
        );
        assert!(set_class_of_device(&runner, "hci9").is_err());
    }
}
