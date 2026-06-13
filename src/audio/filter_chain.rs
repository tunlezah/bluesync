//! The EQ filter-chain as a supervised unit (AUD-023/024/025). Writes the
//! generated PipeWire filter-chain config, then runs the filter-chain process.
//!
//! Correction 1 (UB-6 live-verified): `pipewire -c <module-only-file>` FAILS
//! because passing a config path replaces the base client context (no native
//! protocol). The portable method: write the generated config into
//! `~/.config/pipewire/filter-chain.conf.d/<name>.conf` and run
//! `pipewire -c filter-chain.conf` (the base config merges conf.d/*).
//! If `pipewire-filter-chain` is on PATH, use it directly (Standalone path).
use crate::dsp::eq::{generate_filter_chain_config, EqBand};
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;
use crate::sys::process::{ChildHandle, ProcessSpawner};
use std::io;

/// Config filename written under `<runtime_dir>/soundsync/` (Standalone) or the
/// conf.d directory (Embedded).
pub const FILTER_CHAIN_CONF: &str = "filter-chain.conf";

/// The conf.d filename used for the embedded launch path.
pub const FILTER_CHAIN_CONFD_FILENAME: &str = "soundsync-eq.conf";

/// Directory the filter-chain config is written to for the Standalone path (AUD-023).
pub fn filter_chain_config_dir(runtime_dir: &str) -> String {
    format!("{runtime_dir}/soundsync")
}

/// The conf.d directory where the embedded config is dropped.
/// `~/.config/pipewire/filter-chain.conf.d/`
pub fn filter_chain_confd_path(home: &str) -> String {
    format!("{home}/.config/pipewire/filter-chain.conf.d/{FILTER_CHAIN_CONFD_FILENAME}")
}

/// The conf.d directory (without filename) for use with `Fs::write_atomic`.
pub fn filter_chain_confd_dir(home: &str) -> String {
    format!("{home}/.config/pipewire/filter-chain.conf.d")
}

/// Which binary runs the filter-chain (distros differ — AUD-023).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChainTool {
    /// Standalone `pipewire-filter-chain --config <path>`.
    Standalone,
    /// Embedded conf.d drop + `pipewire -c filter-chain.conf`.
    /// Config is written to `~/.config/pipewire/filter-chain.conf.d/soundsync-eq.conf`;
    /// the base `filter-chain.conf` merges that directory automatically.
    PipewireEmbedded,
}

/// A ready-to-spawn filter-chain command (mirrors `pw::capture_cmd::CaptureCommand`;
/// a future cleanup could unify the two `{program, args}` shapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterChainCommand {
    pub program: &'static str,
    pub args: Vec<String>,
}

/// Build the filter-chain command for the Standalone tool (config path passed explicitly).
pub fn build_filter_chain_command(tool: FilterChainTool, config_path: &str) -> FilterChainCommand {
    match tool {
        FilterChainTool::Standalone => FilterChainCommand {
            program: "pipewire-filter-chain",
            args: vec!["--config".to_string(), config_path.to_string()],
        },
        FilterChainTool::PipewireEmbedded => {
            // The conf.d drop path: `pipewire -c filter-chain.conf`.
            // The base config supplies the client context and merges conf.d/*.
            build_embedded_launch()
        }
    }
}

/// Build the embedded launch command: `pipewire -c filter-chain.conf`.
pub fn build_embedded_launch() -> FilterChainCommand {
    FilterChainCommand {
        program: "pipewire",
        args: vec!["-c".to_string(), "filter-chain.conf".to_string()],
    }
}

/// Detect whether `pipewire-filter-chain` is available on PATH.
pub fn pipewire_filter_chain_available<R: CommandRunner>(runner: &R) -> bool {
    runner
        .run("which", &["pipewire-filter-chain"])
        .map(|o| o.success())
        .unwrap_or(false)
}

/// Detect the appropriate `FilterChainTool` for this host.
/// Prefers Standalone if `pipewire-filter-chain` is on PATH; otherwise Embedded.
pub fn detect_filter_chain_tool<R: CommandRunner>(runner: &R) -> FilterChainTool {
    if pipewire_filter_chain_available(runner) {
        FilterChainTool::Standalone
    } else {
        FilterChainTool::PipewireEmbedded
    }
}

/// Supervised filter-chain unit: write the EQ config atomically, then run the
/// filter-chain process and wait for it to exit. Returns when the process
/// exits (clean or error) — the supervisor restarts it; the dropped child is
/// killed via `kill_on_drop`.
///
/// **Standalone path:** writes to `<runtime_dir>/soundsync/filter-chain.conf`
/// and runs `pipewire-filter-chain --config <path>`.
///
/// **Embedded path (Correction 1):** writes to
/// `<home>/.config/pipewire/filter-chain.conf.d/soundsync-eq.conf` and runs
/// `pipewire -c filter-chain.conf`. The `home` argument is required for the
/// embedded path; it is ignored for Standalone (pass `""` if not needed).
///
/// If the config write fails, the process is NOT spawned (fail fast — better to
/// retry than run against a stale/absent config). The process exit code is
/// intentionally ignored: the supervisor treats every exit as restartable.
pub async fn run_filter_chain<S: ProcessSpawner, F: Fs>(
    spawner: &S,
    fs: &F,
    bands: &[EqBand],
    runtime_dir: &str,
    tool: FilterChainTool,
    home: &str,
) -> io::Result<()> {
    let config = generate_filter_chain_config(bands);

    let cmd = match tool {
        FilterChainTool::Standalone => {
            let dir = filter_chain_config_dir(runtime_dir);
            fs.write_atomic(&dir, FILTER_CHAIN_CONF, &config)?;
            let path = format!("{dir}/{FILTER_CHAIN_CONF}");
            build_filter_chain_command(FilterChainTool::Standalone, &path)
        }
        FilterChainTool::PipewireEmbedded => {
            let dir = filter_chain_confd_dir(home);
            fs.write_atomic(&dir, FILTER_CHAIN_CONFD_FILENAME, &config)?;
            build_embedded_launch()
        }
    };

    let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
    let mut child = spawner.spawn(cmd.program, &args).await?;
    child.wait().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_is_under_runtime_soundsync() {
        assert_eq!(
            filter_chain_config_dir("/run/user/1000"),
            "/run/user/1000/soundsync"
        );
        assert_eq!(FILTER_CHAIN_CONF, "filter-chain.conf");
    }

    #[test]
    fn confd_path_builder() {
        assert_eq!(
            filter_chain_confd_path("/home/mark"),
            "/home/mark/.config/pipewire/filter-chain.conf.d/soundsync-eq.conf"
        );
    }

    #[test]
    fn standalone_command() {
        let c = build_filter_chain_command(FilterChainTool::Standalone, "/p/filter-chain.conf");
        assert_eq!(c.program, "pipewire-filter-chain");
        assert_eq!(c.args, vec!["--config", "/p/filter-chain.conf"]);
    }

    #[test]
    fn embedded_command_via_build_filter_chain_command() {
        // The config_path arg is irrelevant for Embedded — the embedded path always
        // runs `pipewire -c filter-chain.conf`.
        let c = build_filter_chain_command(FilterChainTool::PipewireEmbedded, "/ignored");
        assert_eq!(c.program, "pipewire");
        assert_eq!(c.args, vec!["-c", "filter-chain.conf"]);
    }

    #[test]
    fn embedded_launch_builder() {
        let c = build_embedded_launch();
        assert_eq!(c.program, "pipewire");
        assert_eq!(c.args, vec!["-c", "filter-chain.conf"]);
    }

    #[test]
    fn detect_tool_prefers_standalone_when_available() {
        use crate::sys::command::CommandOutput;
        use crate::sys::testing::FakeCommandRunner;
        let runner = FakeCommandRunner::new().on(
            "which",
            &["pipewire-filter-chain"],
            CommandOutput {
                status: 0,
                stdout: "/usr/bin/pipewire-filter-chain".into(),
                stderr: String::new(),
            },
        );
        assert_eq!(
            detect_filter_chain_tool(&runner),
            FilterChainTool::Standalone
        );
    }

    #[test]
    fn detect_tool_falls_back_to_embedded() {
        use crate::sys::testing::FakeCommandRunner;
        let runner = FakeCommandRunner::new(); // `which` returns 127
        assert_eq!(
            detect_filter_chain_tool(&runner),
            FilterChainTool::PipewireEmbedded
        );
    }

    #[tokio::test]
    async fn run_filter_chain_embedded_writes_to_confd() {
        use crate::dsp::eq::default_bands;
        use crate::sys::testing::{FakeFs, FakeProcessSpawner};

        let fs = FakeFs::new();
        let spawner = FakeProcessSpawner::new(Some(0));
        run_filter_chain(
            &spawner,
            &fs,
            &default_bands(),
            "/run/user/1000",
            FilterChainTool::PipewireEmbedded,
            "/home/mark",
        )
        .await
        .unwrap();

        // Config written to the conf.d path (Correction 1).
        let written = fs
            .written("/home/mark/.config/pipewire/filter-chain.conf.d/soundsync-eq.conf")
            .expect("conf.d config written");
        assert!(written.contains("effect_input.soundsync-eq"));
        assert!(written.contains("node.target = \"soundsync-capture\""));

        // Process spawned as `pipewire -c filter-chain.conf` (base config, not a path).
        let (prog, args) = &spawner.spawns()[0];
        assert_eq!(prog, "pipewire");
        assert_eq!(
            args,
            &vec!["-c".to_string(), "filter-chain.conf".to_string()]
        );
    }

    #[tokio::test]
    async fn run_filter_chain_standalone_writes_to_runtime_dir() {
        use crate::dsp::eq::default_bands;
        use crate::sys::testing::{FakeFs, FakeProcessSpawner};

        let fs = FakeFs::new();
        let spawner = FakeProcessSpawner::new(Some(0));
        run_filter_chain(
            &spawner,
            &fs,
            &default_bands(),
            "/run/user/1000",
            FilterChainTool::Standalone,
            "",
        )
        .await
        .unwrap();

        let written = fs
            .written("/run/user/1000/soundsync/filter-chain.conf")
            .expect("runtime config written");
        assert!(written.contains("effect_input.soundsync-eq"));

        let (prog, args) = &spawner.spawns()[0];
        assert_eq!(prog, "pipewire-filter-chain");
        assert_eq!(
            args,
            &vec![
                "--config".to_string(),
                "/run/user/1000/soundsync/filter-chain.conf".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn run_filter_chain_aborts_if_config_write_fails() {
        use crate::dsp::eq::default_bands;
        use crate::sys::testing::FakeProcessSpawner;

        // An Fs whose write always fails — the process must NOT be spawned.
        struct FailFs;
        impl Fs for FailFs {
            fn read_to_string(&self, _: &str) -> io::Result<String> {
                unimplemented!()
            }
            fn write_atomic(&self, _: &str, _: &str, _: &str) -> io::Result<()> {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "no space"))
            }
            fn remove_file(&self, _: &str, _: &str) -> io::Result<()> {
                unimplemented!()
            }
        }

        let spawner = FakeProcessSpawner::new(Some(0));
        let err = run_filter_chain(
            &spawner,
            &FailFs,
            &default_bands(),
            "/run/user/1000",
            FilterChainTool::Standalone,
            "",
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            spawner.spawns().is_empty(),
            "must not spawn when config write fails"
        );
    }
}
