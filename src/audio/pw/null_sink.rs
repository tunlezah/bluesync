//! Ensure the `soundsync-capture` null sink exists (reuse-or-create, AUD-021/028).
use crate::audio::pw::parse::{parse_all_null_sink_module_ids, parse_null_sink_module_id};
use crate::audio::pw::routing::unload_module;
use crate::sys::command::CommandRunner;
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 10;

/// Result of ensuring the null sink exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NullSink {
    /// Module id, when known (pactl load-module prints it; reuse looks it up).
    pub module_id: Option<u32>,
    /// True if an existing sink was reused rather than newly created.
    pub reused: bool,
}

fn find_existing<R: CommandRunner>(runner: &R, sink_name: &str) -> Option<u32> {
    let out = runner.run("pactl", &["list", "modules"]).ok()?;
    if !out.success() {
        return None;
    }
    parse_null_sink_module_id(&out.stdout, sink_name)
}

/// Reuse the null sink if it already exists; otherwise create it via
/// `pactl load-module module-null-sink`, retrying up to 10× with backoff
/// (pipewire-pulse may not be ready right after boot — AUD-028), then verify it
/// appears in `pactl list short sinks`.
///
/// Async so the retry/backoff yields to the executor instead of blocking the
/// runtime thread (W0.1): the synchronous `Sleeper` would stall the daemon and
/// defeat cooperative shutdown.
pub async fn ensure_null_sink<R: CommandRunner>(
    runner: &R,
    sink_name: &str,
    description: &str,
) -> Result<NullSink, String> {
    if let Some(id) = find_existing(runner, sink_name) {
        return Ok(NullSink {
            module_id: Some(id),
            reused: true,
        });
    }

    let sink_arg = format!("sink_name={sink_name}");
    let props_arg = format!("sink_properties=device.description={description}");
    let mut module_id = None;
    let mut loaded_by_us = false;
    let mut last_err = String::new();

    for attempt in 1..=MAX_ATTEMPTS {
        let out = runner
            .run(
                "pactl",
                &["load-module", "module-null-sink", &sink_arg, &props_arg],
            )
            .map_err(|e| format!("failed to run pactl: {e}"))?;
        if out.success() {
            loaded_by_us = true;
            module_id = out.stdout.trim().parse::<u32>().ok();
            break;
        }
        last_err = out.stderr.trim().to_string();
        if attempt < MAX_ATTEMPTS {
            // Linear backoff: 1s, 2s, ... 5s (capped), then held at 5s.
            tokio::time::sleep(Duration::from_secs((attempt as u64).min(5))).await;
        }
    }

    let verify = runner
        .run("pactl", &["list", "short", "sinks"])
        .map_err(|e| format!("failed to run pactl for verification: {e}"))?;
    if verify.success() && verify.stdout.contains(sink_name) {
        // If our load-module didn't yield a parseable id (or the sink was
        // created by something else), recover the id so the caller can manage
        // it. `reused` reflects whether THIS call created it.
        let id = module_id.or_else(|| find_existing(runner, sink_name));
        return Ok(NullSink {
            module_id: id,
            reused: !loaded_by_us,
        });
    }

    // Verify failed. Distinguish "could never load it" from "loaded but it
    // didn't show up" — the latter would otherwise report an empty last error.
    if loaded_by_us {
        Err(format!(
            "null sink {sink_name} was loaded (module {module_id:?}) but did not appear in `pactl list short sinks`"
        ))
    } else {
        Err(format!(
            "null sink {sink_name} not created after {MAX_ATTEMPTS} attempts; last error: {last_err}"
        ))
    }
}

/// Startup sweep of duplicate `sink_name=<sink_name>` null-sink modules
/// (W2.1 core slice). Given the CANONICAL module id that [`ensure_null_sink`]
/// established as the live sink, unload every OTHER `module-null-sink` whose
/// args carry `sink_name=<sink_name>`, leaving exactly the canonical survivor.
///
/// On PipeWire 1.6 duplicate null sinks share the identical node name
/// `soundsync-capture` (no `.2`/`.3` suffix), so the survivor is disambiguated
/// by MODULE ID, never by name (W0.4 spike). Best-effort: every step logs and
/// continues on error so a stuck duplicate never aborts startup.
///
/// Returns the number of duplicate modules successfully unloaded.
pub fn sweep_duplicate_null_sinks<R: CommandRunner>(
    runner: &R,
    sink_name: &str,
    canonical_module_id: u32,
) -> usize {
    let out = match runner.run("pactl", &["list", "short", "modules"]) {
        Ok(out) if out.success() => out,
        Ok(out) => {
            eprintln!(
                "soundsync: null-sink sweep: `pactl list short modules` failed; skipping: {}",
                out.stderr.trim()
            );
            return 0;
        }
        Err(e) => {
            eprintln!("soundsync: null-sink sweep: could not run pactl; skipping: {e}");
            return 0;
        }
    };

    let mut unloaded = 0;
    for id in parse_all_null_sink_module_ids(&out.stdout, sink_name) {
        if id == canonical_module_id {
            continue;
        }
        match unload_module(runner, id) {
            Ok(()) => {
                eprintln!("soundsync: null-sink sweep: unloaded duplicate module {id}");
                unloaded += 1;
            }
            Err(e) => {
                eprintln!(
                    "soundsync: null-sink sweep: failed to unload duplicate module {id}; continuing: {e}"
                );
            }
        }
    }
    unloaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }
    fn fail(stderr: &str) -> CommandOutput {
        CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    // `start_paused` keeps the backoff sleeps instant under virtual time.

    #[tokio::test(start_paused = true)]
    async fn reuses_existing_null_sink() {
        let modules =
            "Module #7\n\tName: module-null-sink\n\tArgument: sink_name=soundsync-capture\n";
        let runner = FakeCommandRunner::new().on("pactl", &["list", "modules"], ok(modules));
        let r = ensure_null_sink(&runner, "soundsync-capture", "SoundSync-Capture")
            .await
            .unwrap();
        assert_eq!(
            r,
            NullSink {
                module_id: Some(7),
                reused: true
            }
        );
        // No load-module issued when reusing.
        assert!(!runner
            .calls()
            .iter()
            .any(|(p, a)| p == "pactl" && a.iter().any(|x| x == "load-module")));
    }

    #[tokio::test(start_paused = true)]
    async fn creates_when_absent_and_verifies() {
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok("")) // none existing
            .on("pactl", &["load-module"], ok("42")) // created, module id 42
            .on(
                "pactl",
                &["short", "sinks"],
                ok("42\tsoundsync-capture\tPipeWire\tfloat32le 2ch 48000Hz\tSUSPENDED\n"),
            );
        let r = ensure_null_sink(&runner, "soundsync-capture", "SoundSync-Capture")
            .await
            .unwrap();
        assert_eq!(
            r,
            NullSink {
                module_id: Some(42),
                reused: false
            }
        );
        // Loaded on the first attempt -> exactly one load-module call.
        let loads = runner
            .calls()
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "load-module"))
            .count();
        assert_eq!(loads, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn errors_after_max_attempts_when_load_keeps_failing() {
        // load-module always fails; sinks list never shows it.
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok(""))
            .on("pactl", &["load-module"], fail("pa not ready"))
            .on("pactl", &["short", "sinks"], ok("99\tother\n"));
        let err = ensure_null_sink(&runner, "soundsync-capture", "SoundSync-Capture")
            .await
            .unwrap_err();
        assert!(err.contains("soundsync-capture"));
        assert!(err.contains("pa not ready"));
        // 10 attempts (with 9 backoff sleeps in between) -> 10 load-module calls.
        let loads = runner
            .calls()
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "load-module"))
            .count();
        assert_eq!(loads, MAX_ATTEMPTS as usize);
    }

    #[test]
    fn sweep_unloads_every_duplicate_but_the_canonical() {
        // Two duplicate soundsync-capture null sinks (identical name on PW 1.6);
        // canonical is 536870917, so only 536870918 must be unloaded. The
        // other-sink null sink and the loopback are left untouched.
        let short_modules = "\
10\tmodule-something-else\tfoo=bar
536870917\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture
536870918\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture
22\tmodule-null-sink\tsink_name=other-sink
23\tmodule-loopback\tsink=soundsync-capture
";
        let runner = FakeCommandRunner::new()
            .on("pactl", &["short", "modules"], ok(short_modules))
            .on("pactl", &["unload-module"], ok(""));
        let n = sweep_duplicate_null_sinks(&runner, "soundsync-capture", 536870917);
        assert_eq!(n, 1);
        let unloads: Vec<Vec<String>> = runner
            .calls()
            .into_iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "unload-module"))
            .map(|(_, a)| a)
            .collect();
        // Exactly one unload, and it targets the NON-canonical duplicate id.
        assert_eq!(unloads.len(), 1);
        assert!(unloads[0].iter().any(|x| x == "536870918"));
        assert!(!unloads[0].iter().any(|x| x == "536870917"));
    }

    #[test]
    fn sweep_is_noop_when_only_the_canonical_exists() {
        let short_modules =
            "7\tmodule-null-sink\tsink_name=soundsync-capture sink_properties=device.description=SoundSync-Capture\n";
        let runner = FakeCommandRunner::new()
            .on("pactl", &["short", "modules"], ok(short_modules))
            .on("pactl", &["unload-module"], ok(""));
        assert_eq!(
            sweep_duplicate_null_sinks(&runner, "soundsync-capture", 7),
            0
        );
        assert!(!runner
            .calls()
            .iter()
            .any(|(p, a)| p == "pactl" && a.iter().any(|x| x == "unload-module")));
    }

    #[test]
    fn sweep_is_best_effort_on_unload_failure() {
        // Two duplicates besides the canonical; unload always fails -> the sweep
        // attempts both, reports 0 unloaded, and never panics.
        let short_modules = "\
1\tmodule-null-sink\tsink_name=soundsync-capture
2\tmodule-null-sink\tsink_name=soundsync-capture
3\tmodule-null-sink\tsink_name=soundsync-capture
";
        let runner = FakeCommandRunner::new()
            .on("pactl", &["short", "modules"], ok(short_modules))
            .on("pactl", &["unload-module"], fail("busy"));
        assert_eq!(
            sweep_duplicate_null_sinks(&runner, "soundsync-capture", 1),
            0
        );
        let attempts = runner
            .calls()
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "unload-module"))
            .count();
        assert_eq!(attempts, 2); // tried 2 and 3, skipped canonical 1
    }

    #[test]
    fn sweep_skips_when_module_list_unavailable() {
        let runner = FakeCommandRunner::new().on("pactl", &["short", "modules"], fail("no pa"));
        assert_eq!(
            sweep_duplicate_null_sinks(&runner, "soundsync-capture", 7),
            0
        );
        assert!(!runner
            .calls()
            .iter()
            .any(|(p, a)| p == "pactl" && a.iter().any(|x| x == "unload-module")));
    }

    #[tokio::test(start_paused = true)]
    async fn loaded_but_not_verified_reports_accurate_error() {
        // load-module succeeds (id 42) but the sink never shows up in the list.
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok(""))
            .on("pactl", &["load-module"], ok("42"))
            .on("pactl", &["short", "sinks"], ok("99\tother\n"));
        let err = ensure_null_sink(&runner, "soundsync-capture", "SoundSync-Capture")
            .await
            .unwrap_err();
        assert!(err.contains("was loaded"));
        assert!(err.contains("did not appear"));
        // Loaded on the first try -> exactly one load-module call.
        let loads = runner
            .calls()
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "load-module"))
            .count();
        assert_eq!(loads, 1);
    }
}
