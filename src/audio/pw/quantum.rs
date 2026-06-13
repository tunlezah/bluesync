//! PipeWire quantum/rate probe + capture-latency derivation (UB-1, AUD-037).
use crate::sys::command::CommandRunner;

const DEFAULT_QUANTUM: u32 = 1024;
const DEFAULT_RATE: u32 = 48_000;

/// Live PipeWire clock settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PwQuantum {
    pub quantum: u32,
    pub rate: u32,
}

impl PwQuantum {
    /// Capture latency (ms) that safely exceeds the quantum: at least 2× the
    /// quantum period, floored at 50 ms. The legacy broadcast path used a fixed
    /// 20 ms, which is *below* a 1024-frame quantum (21.3 ms) and underruns (UB-1).
    pub fn capture_latency_ms(&self) -> u32 {
        let rate = if self.rate == 0 {
            DEFAULT_RATE
        } else {
            self.rate
        };
        let two_periods_ms = (2u64 * self.quantum as u64 * 1000).div_ceil(rate as u64) as u32;
        two_periods_ms.max(50)
    }
}

/// Parse one `pw-metadata -n settings` value line, e.g.
/// `update: id:0 key:'clock.quantum' value:'1024' type:''` -> 1024.
pub fn parse_pw_setting(output: &str, key: &str) -> Option<u32> {
    let key_pat = format!("key:'{key}'");
    for line in output.lines() {
        if line.contains(&key_pat) {
            let after = line.split("value:'").nth(1)?;
            let value = after.split('\'').next()?;
            return value.trim().parse::<u32>().ok();
        }
    }
    None
}

/// Probe the live PipeWire quantum + rate via `pw-metadata -n settings`; falls
/// back to defaults (1024 @ 48000) if the command fails or can't be parsed.
pub fn probe_quantum<R: CommandRunner>(runner: &R) -> PwQuantum {
    let out = match runner.run("pw-metadata", &["-n", "settings"]) {
        Ok(o) if o.success() => o,
        _ => {
            return PwQuantum {
                quantum: DEFAULT_QUANTUM,
                rate: DEFAULT_RATE,
            };
        }
    };
    PwQuantum {
        quantum: parse_pw_setting(&out.stdout, "clock.quantum").unwrap_or(DEFAULT_QUANTUM),
        rate: parse_pw_setting(&out.stdout, "clock.rate").unwrap_or(DEFAULT_RATE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;

    // Real `pw-metadata -n settings` output shape (from Phase-0 verification).
    const SETTINGS: &str = "\
update: id:0 key:'clock.rate' value:'48000' type:''
update: id:0 key:'clock.quantum' value:'1024' type:''
update: id:0 key:'clock.min-quantum' value:'32' type:''
";

    #[test]
    fn parses_settings_values() {
        assert_eq!(parse_pw_setting(SETTINGS, "clock.quantum"), Some(1024));
        assert_eq!(parse_pw_setting(SETTINGS, "clock.rate"), Some(48000));
        assert_eq!(parse_pw_setting(SETTINGS, "clock.force-quantum"), None);
    }

    #[test]
    fn latency_is_two_quanta_floored_at_50ms() {
        // 1024/48000 = 21.33ms; 2x = 42.67 -> ceil 43 -> floored to 50.
        assert_eq!(
            PwQuantum {
                quantum: 1024,
                rate: 48000
            }
            .capture_latency_ms(),
            50
        );
        // 2048/48000 = 42.67ms; 2x = 85.33 -> 86 (above the 50 floor).
        assert_eq!(
            PwQuantum {
                quantum: 2048,
                rate: 48000
            }
            .capture_latency_ms(),
            86
        );
        // tiny quantum still floored at 50.
        assert_eq!(
            PwQuantum {
                quantum: 256,
                rate: 48000
            }
            .capture_latency_ms(),
            50
        );
        // rate 0 guarded (no divide-by-zero).
        assert_eq!(
            PwQuantum {
                quantum: 1024,
                rate: 0
            }
            .capture_latency_ms(),
            50
        );
        // Exact crossover: 2x quantum period == 50ms (the floor is a no-op here).
        assert_eq!(
            PwQuantum {
                quantum: 1200,
                rate: 48000
            }
            .capture_latency_ms(),
            50
        );
        // One frame above the crossover: 2x takes over.
        assert_eq!(
            PwQuantum {
                quantum: 1201,
                rate: 48000
            }
            .capture_latency_ms(),
            51
        );
    }

    #[test]
    fn probe_returns_parsed_values() {
        let runner = FakeCommandRunner::new().on(
            "pw-metadata",
            &["-n", "settings"],
            CommandOutput {
                status: 0,
                stdout: SETTINGS.into(),
                stderr: String::new(),
            },
        );
        assert_eq!(
            probe_quantum(&runner),
            PwQuantum {
                quantum: 1024,
                rate: 48000
            }
        );
        // Pin the exact invocation: -n settings (default namespace lacks clock.*).
        assert_eq!(runner.calls()[0].1, vec!["-n", "settings"]);
    }

    #[test]
    fn probe_falls_back_to_defaults_on_failure() {
        // unprogrammed runner -> status 127 -> defaults.
        assert_eq!(
            probe_quantum(&FakeCommandRunner::new()),
            PwQuantum {
                quantum: 1024,
                rate: 48000
            }
        );
    }
}
