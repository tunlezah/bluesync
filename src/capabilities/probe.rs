//! Runtime capability probing: decide the WirePlumber config format and read
//! distro identity, via the `sys` effects traits (AUD-017/018).
use crate::capabilities::osrelease::OsRelease;
use crate::capabilities::version::{ConfigFormat, WpVersion};
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;

const OS_RELEASE_PATH: &str = "/etc/os-release";

/// Run `wireplumber --version` and decide the config format. Falls back to Lua
/// (the conservative older-WirePlumber default) if the command fails or the
/// version can't be parsed.
pub fn detect_config_format<R: CommandRunner>(runner: &R) -> ConfigFormat {
    match runner.run("wireplumber", &["--version"]) {
        Ok(out) => WpVersion::parse(&out.stdout)
            .or_else(|| WpVersion::parse(&out.stderr))
            .map(|v| v.config_format())
            .unwrap_or(ConfigFormat::Lua),
        Err(_) => ConfigFormat::Lua,
    }
}

/// Read and parse `/etc/os-release`; returns the default (all-None) on failure.
pub fn detect_os_release<F: Fs>(fs: &F) -> OsRelease {
    fs.read_to_string(OS_RELEASE_PATH)
        .map(|s| OsRelease::parse(&s))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::version::ConfigFormat;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::{FakeCommandRunner, FakeFs};

    fn wp(stdout: &str) -> FakeCommandRunner {
        FakeCommandRunner::new().with(
            "wireplumber",
            CommandOutput {
                status: 0,
                stdout: stdout.into(),
                stderr: String::new(),
            },
        )
    }

    #[test]
    fn detects_spa_json_for_0_5() {
        assert_eq!(
            detect_config_format(&wp("wireplumber 0.5.2")),
            ConfigFormat::SpaJson
        );
    }

    #[test]
    fn detects_lua_for_0_4() {
        assert_eq!(
            detect_config_format(&wp("wireplumber\n0.4.17")),
            ConfigFormat::Lua
        );
    }

    #[test]
    fn falls_back_to_lua_when_version_unparseable_or_nonzero_exit() {
        assert_eq!(detect_config_format(&wp("garbage")), ConfigFormat::Lua);
        // unprogrammed runner -> status 127, empty stdout -> Lua fallback
        assert_eq!(
            detect_config_format(&FakeCommandRunner::new()),
            ConfigFormat::Lua
        );
    }

    #[test]
    fn falls_back_to_lua_when_command_cannot_spawn() {
        // wireplumber binary missing entirely (io::Error) -> Lua fallback.
        let r = FakeCommandRunner::new().with_error("wireplumber");
        assert_eq!(detect_config_format(&r), ConfigFormat::Lua);
    }

    #[test]
    fn reads_version_from_stderr_when_stdout_unparseable() {
        // Realistic: a warning on stdout, the version banner on stderr.
        let r = FakeCommandRunner::new().with(
            "wireplumber",
            CommandOutput {
                status: 0,
                stdout: "Error: some startup warning".into(),
                stderr: "wireplumber 0.5.0".into(),
            },
        );
        assert_eq!(detect_config_format(&r), ConfigFormat::SpaJson);
    }

    #[test]
    fn parses_os_release_when_present() {
        let fs = FakeFs::new().with_file("/etc/os-release", "ID=ubuntu\nVERSION_ID=\"24.04\"\n");
        let os = detect_os_release(&fs);
        assert_eq!(os.id.as_deref(), Some("ubuntu"));
        assert_eq!(os.version_id.as_deref(), Some("24.04"));
    }

    #[test]
    fn os_release_missing_is_default() {
        let os = detect_os_release(&FakeFs::new());
        assert_eq!(os.id, None);
    }
}
