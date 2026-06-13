//! Test fakes for the `sys` traits. `#[cfg(test)]`-only.
use super::command::{CommandOutput, CommandRunner};
use super::fs::Fs;
use super::process::{ChildHandle, ProcessSpawner};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use tokio::io::ReadBuf;

/// Scripted `CommandRunner`: returns a per-program canned `CommandOutput` and
/// records every call for assertions. Unprogrammed programs return status 127;
/// programs registered via `with_error` return an `io::Error` (spawn failure).
#[derive(Default)]
pub struct FakeCommandRunner {
    rules: Vec<(String, Vec<String>, CommandOutput)>,
    responses: HashMap<String, CommandOutput>,
    errors: HashSet<String>,
    calls: RefCell<Vec<(String, Vec<String>)>>,
}

impl FakeCommandRunner {
    pub fn new() -> Self {
        Self::default()
    }
    /// Program a response for `program` (builder style).
    pub fn with(mut self, program: &str, out: CommandOutput) -> Self {
        self.responses.insert(program.to_string(), out);
        self
    }
    /// Make `program` fail to spawn (return `io::Error`), like a missing binary.
    pub fn with_error(mut self, program: &str) -> Self {
        self.errors.insert(program.to_string());
        self
    }
    /// True if `program` was ever run.
    pub fn ran(&self, program: &str) -> bool {
        self.calls.borrow().iter().any(|(p, _)| p == program)
    }
    /// All recorded (program, args) calls.
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        self.calls.borrow().clone()
    }
    /// Program a response matched by program name AND every `must_contain`
    /// value appearing as an EXACT arg of the call (checked before the
    /// per-program `with` fallback, in insertion order). Exact (not substring)
    /// matching avoids surprises like a `set-default` rule catching
    /// `set-default-sink`.
    pub fn on(mut self, program: &str, must_contain: &[&str], out: CommandOutput) -> Self {
        self.rules.push((
            program.to_string(),
            must_contain.iter().map(|s| s.to_string()).collect(),
            out,
        ));
        self
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
        self.calls.borrow_mut().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        if self.errors.contains(program) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("fake: `{program}` failed to spawn"),
            ));
        }
        for (prog, must, out) in &self.rules {
            if prog == program && must.iter().all(|m| args.contains(&m.as_str())) {
                return Ok(out.clone());
            }
        }
        Ok(self
            .responses
            .get(program)
            .cloned()
            .unwrap_or(CommandOutput {
                status: 127,
                stdout: String::new(),
                stderr: format!("fake: no response programmed for `{program}`"),
            }))
    }
}

/// An in-memory `AsyncRead` over a fixed byte buffer; yields the bytes then EOF.
pub struct FakeStdout {
    data: Vec<u8>,
    pos: usize,
}

impl FakeStdout {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, pos: 0 }
    }
}

impl tokio::io::AsyncRead for FakeStdout {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let remaining = &self.data[self.pos..];
        let n = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..n]);
        self.pos += n;
        Poll::Ready(Ok(()))
    }
}

/// A fake child that exits immediately with a programmed code.
pub struct FakeChild {
    code: Option<i32>,
    pub killed: bool,
}

impl ChildHandle for FakeChild {
    async fn wait(&mut self) -> std::io::Result<Option<i32>> {
        Ok(self.code)
    }
    async fn kill(&mut self) -> std::io::Result<()> {
        self.killed = true;
        Ok(())
    }
}

/// A fake spawner: every spawn returns a `FakeChild` with the configured exit
/// code and records the (program, args) call.
pub struct FakeProcessSpawner {
    exit_code: Option<i32>,
    spawns: Mutex<Vec<(String, Vec<String>)>>,
    stdout_bytes: Vec<u8>,
}

impl FakeProcessSpawner {
    pub fn new(exit_code: Option<i32>) -> Self {
        Self {
            exit_code,
            spawns: Mutex::new(Vec::new()),
            stdout_bytes: Vec::new(),
        }
    }
    /// All recorded (program, args) spawn calls.
    pub fn spawns(&self) -> Vec<(String, Vec<String>)> {
        self.spawns.lock().unwrap().clone()
    }
    /// Set the bytes `spawn_capture` stdout will yield (builder style). Every
    /// `spawn_capture` call gets a fresh clone of these bytes (no "pop" semantics).
    pub fn with_stdout(mut self, bytes: Vec<u8>) -> Self {
        self.stdout_bytes = bytes;
        self
    }
}

impl ProcessSpawner for FakeProcessSpawner {
    type Child = FakeChild;
    type Stdout = FakeStdout;
    async fn spawn_capture(
        &self,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<(FakeChild, FakeStdout)> {
        self.spawns.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        Ok((
            FakeChild {
                code: self.exit_code,
                killed: false,
            },
            FakeStdout::new(self.stdout_bytes.clone()),
        ))
    }
    async fn spawn(&self, program: &str, args: &[&str]) -> std::io::Result<FakeChild> {
        self.spawns.lock().unwrap().push((
            program.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        ));
        Ok(FakeChild {
            code: self.exit_code,
            killed: false,
        })
    }
}

/// In-memory `Fs`: stores files by full `dir/filename` path, records removals.
#[derive(Default)]
pub struct FakeFs {
    files: RefCell<HashMap<String, String>>,
    removed: RefCell<Vec<String>>,
}

impl FakeFs {
    pub fn new() -> Self {
        Self::default()
    }
    /// Seed a file at an absolute `path` (builder style).
    pub fn with_file(self, path: &str, contents: &str) -> Self {
        self.files
            .borrow_mut()
            .insert(path.to_string(), contents.to_string());
        self
    }
    /// Contents written to `path`, if any.
    pub fn written(&self, path: &str) -> Option<String> {
        self.files.borrow().get(path).cloned()
    }
    /// True if `path` was passed to `remove_file`.
    pub fn was_removed(&self, path: &str) -> bool {
        self.removed.borrow().iter().any(|p| p == path)
    }
}

impl Fs for FakeFs {
    fn read_to_string(&self, path: &str) -> io::Result<String> {
        self.files
            .borrow()
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fake: no such file"))
    }
    fn write_atomic(&self, dir: &str, filename: &str, contents: &str) -> io::Result<()> {
        self.files
            .borrow_mut()
            .insert(format!("{dir}/{filename}"), contents.to_string());
        Ok(())
    }
    fn remove_file(&self, dir: &str, filename: &str) -> io::Result<()> {
        let p = format!("{dir}/{filename}");
        self.files.borrow_mut().remove(&p);
        self.removed.borrow_mut().push(p);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_runner_records_and_responds() {
        let r = FakeCommandRunner::new().with(
            "wireplumber",
            CommandOutput {
                status: 0,
                stdout: "0.5.2".into(),
                stderr: String::new(),
            },
        );
        let out = r.run("wireplumber", &["--version"]).unwrap();
        assert_eq!(out.stdout, "0.5.2");
        assert!(r.ran("wireplumber"));
        assert_eq!(r.calls()[0].1, vec!["--version".to_string()]);
        // unprogrammed program -> 127
        assert_eq!(r.run("nope", &[]).unwrap().status, 127);
    }

    #[test]
    fn fake_runner_can_inject_spawn_error() {
        let r = FakeCommandRunner::new().with_error("systemctl");
        assert!(r
            .run("systemctl", &["--user", "restart", "wireplumber"])
            .is_err());
        assert!(r.ran("systemctl"));
    }

    #[test]
    fn fake_fs_write_read_remove() {
        let fs = FakeFs::new().with_file("/etc/os-release", "ID=ubuntu\n");
        assert_eq!(fs.read_to_string("/etc/os-release").unwrap(), "ID=ubuntu\n");
        fs.write_atomic("/d", "a.conf", "x").unwrap();
        assert_eq!(fs.written("/d/a.conf").as_deref(), Some("x"));
        fs.remove_file("/d", "a.conf").unwrap();
        assert!(fs.was_removed("/d/a.conf"));
        assert!(fs.read_to_string("/d/a.conf").is_err());
    }

    #[tokio::test]
    async fn fake_spawner_records_and_returns_child() {
        let spawner = FakeProcessSpawner::new(Some(3));
        let mut child = spawner
            .spawn("parec", &["--raw", "--format=float32"])
            .await
            .unwrap();
        assert_eq!(child.wait().await.unwrap(), Some(3));
        child.kill().await.unwrap();
        assert!(child.killed);
        assert_eq!(spawner.spawns()[0].0, "parec");
        assert_eq!(spawner.spawns()[0].1, vec!["--raw", "--format=float32"]);
    }

    #[tokio::test]
    async fn fake_spawn_capture_yields_programmed_stdout() {
        use tokio::io::AsyncReadExt;
        let spawner = FakeProcessSpawner::new(None).with_stdout(b"hello".to_vec());
        let (_child, mut stdout) = spawner.spawn_capture("parec", &["--raw"]).await.unwrap();
        let mut s = String::new();
        stdout.read_to_string(&mut s).await.unwrap();
        assert_eq!(s, "hello");
        assert_eq!(spawner.spawns()[0].0, "parec");
    }

    #[test]
    fn fake_runner_arg_matching_rules_win_over_per_program() {
        let r = FakeCommandRunner::new()
            .on(
                "pactl",
                &["list", "modules"],
                CommandOutput {
                    status: 0,
                    stdout: "MODS".into(),
                    stderr: String::new(),
                },
            )
            .on(
                "pactl",
                &["short", "sinks"],
                CommandOutput {
                    status: 0,
                    stdout: "SINKS".into(),
                    stderr: String::new(),
                },
            );
        assert_eq!(r.run("pactl", &["list", "modules"]).unwrap().stdout, "MODS");
        assert_eq!(
            r.run("pactl", &["list", "short", "sinks"]).unwrap().stdout,
            "SINKS"
        );
        // no matching rule -> 127
        assert_eq!(r.run("pactl", &["info"]).unwrap().status, 127);
    }
}
