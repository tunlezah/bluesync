//! Async process-spawning seam. Real impl over `tokio::process`; the test fake
//! lives in `sys::testing`. Static dispatch only (associated type + RPITIT with
//! Send) — no async-trait, no dyn.
use std::future::Future;
use std::io;
use std::process::Stdio;

/// A handle to a spawned child process.
pub trait ChildHandle: Send {
    /// Await the child's exit; returns its exit code if known.
    fn wait(&mut self) -> impl Future<Output = io::Result<Option<i32>>> + Send;
    /// Kill the child (best-effort, idempotent enough for shutdown).
    fn kill(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

/// Spawns long-lived child processes. Generic consumers (`<S: ProcessSpawner>`)
/// keep this testable with a fake; we never need `dyn` heterogeneity.
pub trait ProcessSpawner: Send + Sync {
    type Child: ChildHandle;
    /// Byte stream of a capture child's stdout.
    type Stdout: tokio::io::AsyncRead + Unpin + Send;

    /// Spawn a process with its stdout piped, returning the child handle and a
    /// reader over its stdout (used by the capture unit to read PCM).
    fn spawn_capture(
        &self,
        program: &str,
        args: &[&str],
    ) -> impl Future<Output = io::Result<(Self::Child, Self::Stdout)>> + Send;

    /// Spawn a process with inherited stdio (for children whose output we don't
    /// read, e.g. the EQ filter-chain). Use `spawn_capture` when stdout is needed.
    fn spawn(
        &self,
        program: &str,
        args: &[&str],
    ) -> impl Future<Output = io::Result<Self::Child>> + Send;
}

/// Real spawner over `tokio::process`.
pub struct TokioProcessSpawner;

/// Real child wrapping `tokio::process::Child`.
pub struct TokioChild {
    child: tokio::process::Child,
}

impl ChildHandle for TokioChild {
    async fn wait(&mut self) -> io::Result<Option<i32>> {
        Ok(self.child.wait().await?.code())
    }
    async fn kill(&mut self) -> io::Result<()> {
        self.child.kill().await
    }
}

impl ProcessSpawner for TokioProcessSpawner {
    type Child = TokioChild;
    type Stdout = tokio::process::ChildStdout;
    async fn spawn_capture(
        &self,
        program: &str,
        args: &[&str],
    ) -> io::Result<(TokioChild, tokio::process::ChildStdout)> {
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("spawn_capture: child stdout was not piped"))?;
        Ok((TokioChild { child }, stdout))
    }
    async fn spawn(&self, program: &str, args: &[&str]) -> io::Result<TokioChild> {
        let child = tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .spawn()?;
        Ok(TokioChild { child })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn real_spawner_runs_and_reports_exit_code() {
        let spawner = TokioProcessSpawner;
        let mut child = spawner.spawn("sh", &["-c", "exit 7"]).await.unwrap();
        assert_eq!(child.wait().await.unwrap(), Some(7));
    }

    #[tokio::test]
    async fn real_spawner_kill_is_ok() {
        let spawner = TokioProcessSpawner;
        let mut child = spawner.spawn("sh", &["-c", "sleep 100"]).await.unwrap();
        child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn spawn_capture_pipes_stdout() {
        use tokio::io::AsyncReadExt;
        let spawner = TokioProcessSpawner;
        let (_child, mut stdout) = spawner
            .spawn_capture("sh", &["-c", "printf abcd"])
            .await
            .unwrap();
        let mut s = String::new();
        stdout.read_to_string(&mut s).await.unwrap();
        assert_eq!(s, "abcd");
    }
}
