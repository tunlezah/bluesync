//! Read-only self-check (`soundsync doctor`). All I/O goes through the
//! `CommandRunner` / `Fs` seams so the checks are fully unit-testable.
use crate::capabilities::probe::detect_config_format;
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;
use crate::wireplumber::config::generate;
use std::fmt;

/// Outcome of a single doctor check.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckStatus {
    /// Everything looks good.
    Pass,
    /// Something is missing or suspicious, but the daemon can still run.
    Warn,
    /// A hard requirement is absent; the daemon will not work until fixed.
    Fail,
}

/// A single named doctor check with its outcome and a human-readable detail.
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

/// Collection of checks returned by `run_checks`.
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// `true` iff no check has status `Fail`.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Fail)
    }
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in &self.checks {
            let icon = match c.status {
                CheckStatus::Pass => '✓',
                CheckStatus::Warn => '⚠',
                CheckStatus::Fail => '✗',
            };
            writeln!(f, "{icon} [{}] {}", c.name, c.detail)?;
        }
        Ok(())
    }
}

/// Required binaries the daemon depends on.
const REQUIRED_BINS: &[&str] = &[
    "parec",
    "pw-cat",
    "pactl",
    "wpctl",
    "pw-link",
    "pw-metadata",
    "ffmpeg",
    "hciconfig",
    "pipewire",
    "wireplumber",
];

/// Run all doctor checks over the injected runner/fs seams.
///
/// `web_dir` is the resolved web-UI directory path (e.g. from
/// `config::resolve_web_dir`). The function is synchronous and makes no
/// assumptions about process state.
pub fn run_checks<R: CommandRunner, F: Fs>(runner: &R, fs: &F, web_dir: &str) -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Required binaries ─────────────────────────────────────────────────
    {
        let mut missing: Vec<&str> = Vec::new();
        for &bin in REQUIRED_BINS {
            let cmd = format!("command -v {bin}");
            match runner.run("sh", &["-c", &cmd]) {
                Ok(out) if out.success() => {}
                _ => missing.push(bin),
            }
        }
        if missing.is_empty() {
            checks.push(Check {
                name: "binaries".to_string(),
                status: CheckStatus::Pass,
                detail: "all required binaries present".to_string(),
            });
        } else {
            checks.push(Check {
                name: "binaries".to_string(),
                status: CheckStatus::Fail,
                detail: format!("missing: {}", missing.join(", ")),
            });
        }
    }

    // 2. PipeWire running ──────────────────────────────────────────────────
    {
        match runner.run("pactl", &["info"]) {
            Ok(out) if out.success() => {
                checks.push(Check {
                    name: "pipewire-running".to_string(),
                    status: CheckStatus::Pass,
                    detail: "PipeWire reachable via pactl".to_string(),
                });
            }
            _ => {
                checks.push(Check {
                    name: "pipewire-running".to_string(),
                    status: CheckStatus::Fail,
                    detail: "PipeWire not reachable; is the user session up?".to_string(),
                });
            }
        }
    }

    // 3. WP config present + format ────────────────────────────────────────
    {
        let wp_fmt = detect_config_format(runner);
        let cfg = generate(wp_fmt);
        let path = format!("{}/{}", cfg.etc_dir, cfg.filename);
        match fs.read_to_string(&path) {
            Ok(_) => {
                checks.push(Check {
                    name: "wp-config".to_string(),
                    status: CheckStatus::Pass,
                    detail: format!("WirePlumber A2DP config present at {path}"),
                });
            }
            Err(_) => {
                checks.push(Check {
                    name: "wp-config".to_string(),
                    status: CheckStatus::Fail,
                    detail: format!(
                        "WirePlumber A2DP config absent at {path}; run: sudo soundsync apply-wireplumber-config"
                    ),
                });
            }
        }
    }

    // 4. Bluetooth adapter present ─────────────────────────────────────────
    {
        match runner.run("hciconfig", &["hci0"]) {
            Ok(out) if out.success() => {
                checks.push(Check {
                    name: "bt-adapter".to_string(),
                    status: CheckStatus::Pass,
                    detail: "hci0 adapter present".to_string(),
                });
            }
            _ => {
                checks.push(Check {
                    name: "bt-adapter".to_string(),
                    status: CheckStatus::Warn,
                    detail: "no hci0 adapter".to_string(),
                });
            }
        }
    }

    // 5. Web UI index.html exists ──────────────────────────────────────────
    {
        let index = format!("{web_dir}/index.html");
        match fs.read_to_string(&index) {
            Ok(_) => {
                checks.push(Check {
                    name: "web-dir".to_string(),
                    status: CheckStatus::Pass,
                    detail: format!("SPA found at {index}"),
                });
            }
            Err(_) => {
                checks.push(Check {
                    name: "web-dir".to_string(),
                    status: CheckStatus::Warn,
                    detail: format!("SPA not found at {web_dir}; set SOUNDSYNC_WEB_DIR"),
                });
            }
        }
    }

    DoctorReport { checks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::{FakeCommandRunner, FakeFs};

    /// Build a runner that makes all required binaries "present" and pactl/hciconfig succeed.
    fn all_ok_runner() -> FakeCommandRunner {
        // All `command -v <bin>` calls go to "sh" — one per binary.
        // We use the `.with` fallback: sh returns success for anything,
        // pactl info succeeds, hciconfig hci0 succeeds, wireplumber --version
        // returns a recognisable version so WP config path can be computed.
        FakeCommandRunner::new()
            .with(
                "sh",
                CommandOutput {
                    status: 0,
                    stdout: "/usr/bin/binary".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "wireplumber",
                CommandOutput {
                    status: 0,
                    stdout: "wireplumber 0.5.2".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "pactl",
                CommandOutput {
                    status: 0,
                    stdout: "Server Name: PipeWire".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "hciconfig",
                CommandOutput {
                    status: 0,
                    stdout: "hci0: ...".to_string(),
                    stderr: String::new(),
                },
            )
    }

    /// Build an Fs with the WP SPA-JSON config path and a web-dir index.html.
    fn all_ok_fs() -> FakeFs {
        FakeFs::new()
            .with_file(
                "/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf",
                "# config",
            )
            .with_file("/usr/share/soundsync/webui/index.html", "<!DOCTYPE html>")
    }

    #[test]
    fn all_present_report_is_ok() {
        let runner = all_ok_runner();
        let fs = all_ok_fs();
        let report = run_checks(&runner, &fs, "/usr/share/soundsync/webui");
        assert!(
            report.ok(),
            "all-present report should be ok; checks: {:?}",
            report
                .checks
                .iter()
                .map(|c| (&c.name, &c.status, &c.detail))
                .collect::<Vec<_>>()
        );
        assert_eq!(report.checks.len(), 5);
        assert!(report.checks.iter().all(|c| c.status == CheckStatus::Pass));
    }

    #[test]
    fn missing_binary_gives_fail() {
        // sh returns status 1 (command -v not found) for all commands
        let runner = FakeCommandRunner::new()
            .with(
                "sh",
                CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: String::new(),
                },
            )
            .with(
                "wireplumber",
                CommandOutput {
                    status: 0,
                    stdout: "wireplumber 0.5.2".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "pactl",
                CommandOutput {
                    status: 0,
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "hciconfig",
                CommandOutput {
                    status: 0,
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                },
            );
        let fs = all_ok_fs();
        let report = run_checks(&runner, &fs, "/usr/share/soundsync/webui");

        let bin_check = report
            .checks
            .iter()
            .find(|c| c.name == "binaries")
            .expect("binaries check must exist");
        assert_eq!(bin_check.status, CheckStatus::Fail);
        assert!(!report.ok());
    }

    #[test]
    fn missing_wp_config_gives_fail() {
        let runner = all_ok_runner();
        // fs has no WP config file (empty)
        let fs =
            FakeFs::new().with_file("/usr/share/soundsync/webui/index.html", "<!DOCTYPE html>");
        let report = run_checks(&runner, &fs, "/usr/share/soundsync/webui");

        let wp_check = report
            .checks
            .iter()
            .find(|c| c.name == "wp-config")
            .expect("wp-config check must exist");
        assert_eq!(wp_check.status, CheckStatus::Fail);
        assert!(wp_check.detail.contains("apply-wireplumber-config"));
        assert!(!report.ok());
    }

    #[test]
    fn missing_bt_adapter_gives_warn_not_fail() {
        let runner = FakeCommandRunner::new()
            .with(
                "sh",
                CommandOutput {
                    status: 0,
                    stdout: "/usr/bin/x".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "wireplumber",
                CommandOutput {
                    status: 0,
                    stdout: "wireplumber 0.5.2".to_string(),
                    stderr: String::new(),
                },
            )
            .with(
                "pactl",
                CommandOutput {
                    status: 0,
                    stdout: "ok".to_string(),
                    stderr: String::new(),
                },
            )
            // hciconfig is NOT programmed → status 127 (adapter absent)
            ;
        let fs = all_ok_fs();
        let report = run_checks(&runner, &fs, "/usr/share/soundsync/webui");

        let bt_check = report
            .checks
            .iter()
            .find(|c| c.name == "bt-adapter")
            .expect("bt-adapter check must exist");
        assert_eq!(bt_check.status, CheckStatus::Warn);
        // report.ok() must still be true — Warn doesn't fail
        assert!(
            report.ok(),
            "missing adapter is only a Warn; report should be ok"
        );
    }

    #[test]
    fn missing_web_dir_gives_warn_not_fail() {
        let runner = all_ok_runner();
        // fs has WP config but NO webui index.html
        let fs = FakeFs::new().with_file(
            "/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf",
            "# config",
        );
        let report = run_checks(&runner, &fs, "/usr/share/soundsync/webui");

        let web_check = report
            .checks
            .iter()
            .find(|c| c.name == "web-dir")
            .expect("web-dir check must exist");
        assert_eq!(web_check.status, CheckStatus::Warn);
        assert!(web_check.detail.contains("SOUNDSYNC_WEB_DIR"));
        assert!(
            report.ok(),
            "missing web dir is only a Warn; report should be ok"
        );
    }

    #[test]
    fn ok_logic_warn_does_not_fail_fail_does() {
        // Construct a synthetic report to test ok() directly.
        let report_with_warn = DoctorReport {
            checks: vec![Check {
                name: "test".to_string(),
                status: CheckStatus::Warn,
                detail: "something".to_string(),
            }],
        };
        assert!(report_with_warn.ok());

        let report_with_fail = DoctorReport {
            checks: vec![Check {
                name: "test".to_string(),
                status: CheckStatus::Fail,
                detail: "something".to_string(),
            }],
        };
        assert!(!report_with_fail.ok());
    }

    #[test]
    fn display_format_uses_icons() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    name: "a".to_string(),
                    status: CheckStatus::Pass,
                    detail: "all good".to_string(),
                },
                Check {
                    name: "b".to_string(),
                    status: CheckStatus::Warn,
                    detail: "watch out".to_string(),
                },
                Check {
                    name: "c".to_string(),
                    status: CheckStatus::Fail,
                    detail: "broken".to_string(),
                },
            ],
        };
        let s = report.to_string();
        assert!(s.contains('✓'), "Pass should use ✓");
        assert!(s.contains('⚠'), "Warn should use ⚠");
        assert!(s.contains('✗'), "Fail should use ✗");
        assert!(s.contains("[a]"));
        assert!(s.contains("[b]"));
        assert!(s.contains("[c]"));
    }
}
