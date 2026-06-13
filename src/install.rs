//! Install-time privileged subcommands. These run as root (via the package
//! postinst or the root oneshot systemd service) and call into the existing
//! `wireplumber::apply` and `bluetooth::adapter` modules. They are thin glue
//! that surface errors as human-readable strings (suitable for eprintln! in
//! `main`) rather than `io::Error`.
use crate::bluetooth::adapter::{set_class_of_device, set_hci_name};
use crate::capabilities::probe::detect_config_format;
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;
use crate::wireplumber::apply::apply_config;

/// Write the version-correct WirePlumber A2DP config and attempt to restart
/// WirePlumber. Returns a human-readable summary on success.
///
/// This runs as **root** at install time (writes `/etc/wireplumber/...`).
///
/// Signature note: `apply_config(format, fs, runner)` — format is the first arg
/// (see `wireplumber::apply::apply_config`).
pub fn apply_wireplumber_config<F: Fs, R: CommandRunner>(
    fs: &F,
    runner: &R,
) -> Result<String, String> {
    let fmt = detect_config_format(runner);
    let applied = apply_config(fmt, fs, runner)
        .map_err(|e| format!("failed to write WirePlumber config: {e}"))?;
    let summary = format!(
        "wrote: {}\nremoved stale: {}\nwireplumber restarted: {}",
        applied.written_path, applied.removed_stale_path, applied.restarted
    );
    Ok(summary)
}

/// Set the Bluetooth adapter's Class-of-Device (0x240414) and HCI-level name.
///
/// This runs as **root** in the boot-time oneshot `soundsync-adapter.service`
/// (`After=bluetooth.target`). Errors are surfaced so they appear in the unit's
/// journal.
pub fn apply_adapter_config<R: CommandRunner>(
    runner: &R,
    hci: &str,
    name: &str,
) -> Result<(), String> {
    set_class_of_device(runner, hci)?;
    set_hci_name(runner, hci, name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::{FakeCommandRunner, FakeFs};

    fn ok_out() -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn wp05_runner() -> FakeCommandRunner {
        FakeCommandRunner::new()
            .with(
                "wireplumber",
                CommandOutput {
                    status: 0,
                    stdout: "wireplumber 0.5.2".to_string(),
                    stderr: String::new(),
                },
            )
            .with("systemctl", ok_out())
    }

    #[test]
    fn apply_wireplumber_config_returns_summary() {
        let runner = wp05_runner();
        let fs = FakeFs::new();
        let summary = apply_wireplumber_config(&fs, &runner).expect("should succeed");
        assert!(summary.contains("wrote:"), "summary: {summary}");
        assert!(summary.contains("removed stale:"), "summary: {summary}");
        assert!(
            summary.contains("wireplumber restarted:"),
            "summary: {summary}"
        );
        // the written path should be the 0.5 SPA-JSON conf
        assert!(summary.contains("51-soundsync.conf"), "summary: {summary}");
    }

    #[test]
    fn apply_adapter_config_sets_cod_and_name() {
        let runner = FakeCommandRunner::new().with("hciconfig", ok_out());
        apply_adapter_config(&runner, "hci0", "SoundSync").expect("should succeed");

        let calls = runner.calls();
        // First call: set CoD
        assert_eq!(calls[0].0, "hciconfig");
        assert!(calls[0].1.contains(&"class".to_string()));
        // Second call: set name
        assert_eq!(calls[1].0, "hciconfig");
        assert!(calls[1].1.contains(&"name".to_string()));
        assert!(calls[1].1.contains(&"SoundSync".to_string()));
    }

    #[test]
    fn apply_adapter_config_propagates_cod_error() {
        let runner = FakeCommandRunner::new().with(
            "hciconfig",
            CommandOutput {
                status: 1,
                stdout: String::new(),
                stderr: "no such device".to_string(),
            },
        );
        let err = apply_adapter_config(&runner, "hci9", "SoundSync").unwrap_err();
        assert!(err.contains("hciconfig"), "err: {err}");
    }
}
