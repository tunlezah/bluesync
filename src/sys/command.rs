//! Running external programs (pactl, pw-link, wireplumber, systemctl, ...).
use std::io;

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
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        let out = std::process::Command::new(program).args(args).output()?;
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
}
