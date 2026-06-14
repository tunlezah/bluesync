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

/// Like [`detect_config_format`], but when `wireplumber --version` is unreadable
/// (spawn failure, non-zero exit, or unparseable banner) it does NOT hard-assume
/// Lua. Instead it inspects BOTH candidate on-disk config paths and prefers the
/// format whose file actually exists, so a momentarily-unreachable WirePlumber
/// cannot drive a false FAIL by making doctor expect the wrong path (G10).
///
/// Resolution order:
/// 1. If the version parses, trust it (authoritative — matches the writer).
/// 2. Else if exactly one candidate config file exists on disk, use that format.
/// 3. Else fall back to Lua (the original conservative default).
///
/// This is read-only and used by `soundsync doctor`; the install/write path
/// continues to use [`detect_config_format`] (where Lua is the safe default).
pub fn detect_config_format_guarded<R: CommandRunner, F: Fs>(runner: &R, fs: &F) -> ConfigFormat {
    if let Ok(out) = runner.run("wireplumber", &["--version"]) {
        if let Some(v) = WpVersion::parse(&out.stdout).or_else(|| WpVersion::parse(&out.stderr)) {
            return v.config_format();
        }
    }
    // Version unreadable: probe both candidate paths before assuming a format.
    let spa = generate_path(ConfigFormat::SpaJson);
    let lua = generate_path(ConfigFormat::Lua);
    let spa_present = fs.read_to_string(&spa).is_ok();
    let lua_present = fs.read_to_string(&lua).is_ok();
    match (spa_present, lua_present) {
        (true, false) => ConfigFormat::SpaJson,
        (false, true) => ConfigFormat::Lua,
        // Neither present (nothing to disambiguate) or both present (ambiguous):
        // keep the original conservative Lua default.
        _ => ConfigFormat::Lua,
    }
}

/// Full `dir/filename` path the writer would use for `format` (oracle:
/// `wireplumber::config::generate`).
fn generate_path(format: ConfigFormat) -> String {
    let cfg = crate::wireplumber::config::generate(format);
    format!("{}/{}", cfg.etc_dir, cfg.filename)
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

    // ── detect_config_format_guarded (G10 probe guard) ──────────────────────

    const SPA_PATH: &str = "/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf";
    const LUA_PATH: &str = "/etc/wireplumber/bluetooth.lua.d/51-soundsync-a2dp.lua";

    #[test]
    fn guarded_trusts_parseable_version_over_disk() {
        // Version parses as 0.5 → SpaJson, even though a stale Lua file is present.
        let r = wp("wireplumber 0.5.2");
        let fs = FakeFs::new().with_file(LUA_PATH, "-- stale");
        assert_eq!(detect_config_format_guarded(&r, &fs), ConfigFormat::SpaJson);
    }

    #[test]
    fn guarded_prefers_existing_spa_file_when_version_unreadable() {
        // wireplumber missing entirely; only the SPA-JSON file exists on disk.
        let r = FakeCommandRunner::new().with_error("wireplumber");
        let fs = FakeFs::new().with_file(SPA_PATH, "# config");
        assert_eq!(
            detect_config_format_guarded(&r, &fs),
            ConfigFormat::SpaJson,
            "must not hard-assume Lua when the SPA file is the one on disk"
        );
    }

    #[test]
    fn guarded_prefers_existing_lua_file_when_version_unreadable() {
        let r = FakeCommandRunner::new(); // status 127, empty output → unreadable
        let fs = FakeFs::new().with_file(LUA_PATH, "-- config");
        assert_eq!(detect_config_format_guarded(&r, &fs), ConfigFormat::Lua);
    }

    #[test]
    fn guarded_falls_back_to_lua_when_unreadable_and_no_file() {
        let r = FakeCommandRunner::new().with_error("wireplumber");
        let fs = FakeFs::new();
        assert_eq!(detect_config_format_guarded(&r, &fs), ConfigFormat::Lua);
    }

    #[test]
    fn guarded_ambiguous_both_present_keeps_lua_default() {
        let r = FakeCommandRunner::new().with_error("wireplumber");
        let fs = FakeFs::new()
            .with_file(SPA_PATH, "# config")
            .with_file(LUA_PATH, "-- config");
        assert_eq!(detect_config_format_guarded(&r, &fs), ConfigFormat::Lua);
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
