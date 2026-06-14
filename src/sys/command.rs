//! Running external programs (pactl, pw-link, wireplumber, systemctl, ...).
use std::io;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Default per-command timeout. A wedged helper (e.g. a `pactl` blocked on a
/// hung PipeWire socket) must not stall reconciliation forever. Sized above the
/// per-call null-sink backoff cap (5s) and below the unit `TimeoutStopSec` (15s).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to sleep between `try_wait` polls while waiting on the child.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Resolve the per-command timeout from `SOUNDSYNC_COMMAND_TIMEOUT` (seconds),
/// falling back to [`COMMAND_TIMEOUT`]. An unparseable or zero value falls back
/// to the default (a zero timeout would kill every command instantly). Mirrors
/// `main::resolve_shutdown_timeout`. Read once and cached so the hot reconcile
/// loop does not touch the environment on every call.
fn command_timeout() -> Duration {
    static TIMEOUT: OnceLock<Duration> = OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        let secs = std::env::var("SOUNDSYNC_COMMAND_TIMEOUT")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|&s| s > 0);
        match secs {
            Some(s) => Duration::from_secs(s),
            None => COMMAND_TIMEOUT,
        }
    })
}

/// Captured result of running a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    /// True iff the program exited 0.
    pub fn success(&self) -> bool {
        self.status == 0
    }
}

/// Runs an external program (argv form — no shell). Abstracted so integration
/// logic can be unit-tested with a fake instead of spawning real processes.
pub trait CommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput>;
}

/// Real implementation over `std::process::Command`.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    /// Spawn the child and wait on it with a deadline (see [`command_timeout`]).
    /// On timeout the child is killed + reaped and an `io::Error` is returned.
    ///
    /// IMPORTANT (failure shape; NF-6 depends on this): a timeout surfaces as a
    /// normal command error — the SAME `Err(io::Error)` shape as a spawn failure
    /// — NOT a panic and NOT a successful `CommandOutput` with a magic status.
    /// Every caller already treats an `Err` (or non-zero exit) as "command
    /// failed", so a wedged helper degrades gracefully instead of `?`-aborting
    /// startup or hanging the reconcile loop. The reuse-verify path in
    /// `null_sink.rs` must keep-reuse on this `Err` rather than propagate it.
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        use std::process::{Command, Stdio};

        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let deadline = Instant::now() + command_timeout();
        loop {
            match child.try_wait()? {
                Some(_status) => break,
                None => {
                    if Instant::now() >= deadline {
                        // Kill BEFORE draining the pipes: a wedged child may have
                        // filled (or be blocked on) its stdout/stderr buffer, so
                        // reading first could itself deadlock.
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("`{program}` timed out after {:?}", command_timeout()),
                        ));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        }

        // Completed within the deadline: collect the captured output. The helper
        // outputs we shell out to (pactl/wpctl/which/hostname/...) are small, so
        // reading the pipes after exit cannot deadlock.
        let out = child.wait_with_output()?;
        Ok(CommandOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_success_is_status_zero() {
        let ok = CommandOutput {
            status: 0,
            stdout: "x".into(),
            stderr: String::new(),
        };
        let bad = CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "e".into(),
        };
        assert!(ok.success());
        assert!(!bad.success());
    }

    #[test]
    fn real_runner_captures_output_and_status() {
        let r = SystemCommandRunner;
        let out = r.run("sh", &["-c", "printf hello; exit 0"]).unwrap();
        assert_eq!(out.status, 0);
        assert_eq!(out.stdout, "hello");
        assert!(out.success());
    }

    #[test]
    fn real_runner_times_out_and_kills_wedged_child() {
        // Prefer a tiny override so the test is fast; if some earlier test in
        // this binary already cached `command_timeout()`, fall back to asserting
        // against whatever it resolved to (still bounded by the 10s default).
        std::env::set_var("SOUNDSYNC_COMMAND_TIMEOUT", "1");
        let limit = command_timeout();
        std::env::remove_var("SOUNDSYNC_COMMAND_TIMEOUT");

        let r = SystemCommandRunner;
        let start = Instant::now();
        // `sleep 30` far exceeds the timeout; the runner must give up early.
        let res = r.run("sleep", &["30"]);
        let elapsed = start.elapsed();

        // Timeout surfaces as a normal command error (NOT a panic, NOT an Ok).
        let err = res.expect_err("wedged child must yield an Err, not Ok");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        // Returned well before `sleep 30` would have finished (and before the
        // child was reaped via blocking wait), proving the child was killed.
        assert!(
            elapsed < limit + Duration::from_secs(5),
            "timed out too late: {elapsed:?} (limit {limit:?})"
        );
        assert!(
            elapsed < Duration::from_secs(28),
            "did not preempt `sleep 30`: {elapsed:?}"
        );
    }
}
