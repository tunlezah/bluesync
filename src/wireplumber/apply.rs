//! Apply the version-correct WirePlumber A2DP config (AUD-017/018/019):
//! remove the wrong-format stale file, write the correct one atomically, then
//! restart + report WirePlumber liveness.
use crate::capabilities::version::ConfigFormat;
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;
use crate::wireplumber::config::{generate, WpConfigFile};
use std::io;

/// What `apply_config` did, for logging/verification by callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedConfig {
    pub written_path: String,
    pub removed_stale_path: String,
    pub restarted: bool,
}

fn opposite(format: ConfigFormat) -> ConfigFormat {
    match format {
        ConfigFormat::Lua => ConfigFormat::SpaJson,
        ConfigFormat::SpaJson => ConfigFormat::Lua,
    }
}

fn full_path(c: &WpConfigFile) -> String {
    format!("{}/{}", c.etc_dir, c.filename)
}

/// Remove the wrong-format stale config, write the correct config atomically,
/// then restart WirePlumber. The config write is durable independently of the
/// restart: a restart that exits non-zero OR fails to spawn at all is reported
/// as `restarted: false`, never as an error from this function.
pub fn apply_config<F: Fs, R: CommandRunner>(
    format: ConfigFormat,
    fs: &F,
    runner: &R,
) -> io::Result<AppliedConfig> {
    let cfg = generate(format);
    let stale = generate(opposite(format));

    // A .conf is silently ignored by WP 0.4 and a .lua by WP 0.5 — remove the
    // wrong-format file so a stale config can't masquerade as "configured".
    fs.remove_file(stale.etc_dir, &stale.filename)?;
    fs.write_atomic(cfg.etc_dir, &cfg.filename, &cfg.contents)?;

    // The write above is the durable, load-bearing effect. A failed restart
    // (non-zero exit OR spawn failure) must not undo it — WirePlumber will pick
    // the config up on its next restart/boot.
    let restarted = match runner.run("systemctl", &["--user", "restart", "wireplumber"]) {
        Ok(out) => out.success(),
        Err(_) => false,
    };

    Ok(AppliedConfig {
        written_path: full_path(&cfg),
        removed_stale_path: full_path(&stale),
        restarted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::version::ConfigFormat;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::{FakeCommandRunner, FakeFs};

    fn ok_systemctl() -> FakeCommandRunner {
        FakeCommandRunner::new().with(
            "systemctl",
            CommandOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        )
    }

    #[test]
    fn applies_spa_json_writes_conf_removes_stale_lua_restarts() {
        let fs = FakeFs::new();
        let runner = ok_systemctl();
        let applied = apply_config(ConfigFormat::SpaJson, &fs, &runner).unwrap();

        assert_eq!(
            applied.written_path,
            "/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf"
        );
        let written = fs.written(&applied.written_path).unwrap();
        assert!(written.contains("monitor.bluez.seat-monitoring = disabled"));

        assert_eq!(
            applied.removed_stale_path,
            "/etc/wireplumber/bluetooth.lua.d/51-soundsync-a2dp.lua"
        );
        assert!(fs.was_removed(&applied.removed_stale_path));

        // Exact restart invocation (guards against a --user -> --system regression).
        assert_eq!(
            runner.calls()[0].1,
            vec!["--user", "restart", "wireplumber"]
        );
        assert!(applied.restarted);
    }

    #[test]
    fn applies_lua_writes_lua_removes_stale_conf() {
        let fs = FakeFs::new();
        let runner = ok_systemctl();
        let applied = apply_config(ConfigFormat::Lua, &fs, &runner).unwrap();

        assert_eq!(
            applied.written_path,
            "/etc/wireplumber/bluetooth.lua.d/51-soundsync-a2dp.lua"
        );
        let written = fs.written(&applied.written_path).unwrap();
        assert!(written.contains("bluez_monitor.properties[\"bluez5.roles\"] = \"[ a2dp_sink ]\""));

        assert_eq!(
            applied.removed_stale_path,
            "/etc/wireplumber/wireplumber.conf.d/51-soundsync.conf"
        );
        assert!(fs.was_removed(&applied.removed_stale_path));
    }

    #[test]
    fn restart_nonzero_exit_is_reported_not_fatal() {
        let fs = FakeFs::new();
        // unprogrammed runner -> systemctl returns 127 -> restarted=false, but write still happened
        let runner = FakeCommandRunner::new();
        let applied = apply_config(ConfigFormat::SpaJson, &fs, &runner).unwrap();
        assert!(!applied.restarted);
        assert!(fs.written(&applied.written_path).is_some());
    }

    #[test]
    fn restart_spawn_failure_is_not_fatal_and_write_persists() {
        let fs = FakeFs::new();
        // systemctl cannot even spawn (io::Error) -> apply_config still Ok,
        // restarted=false, and the config is durably written.
        let runner = FakeCommandRunner::new().with_error("systemctl");
        let applied = apply_config(ConfigFormat::SpaJson, &fs, &runner).unwrap();
        assert!(!applied.restarted);
        assert!(fs.written(&applied.written_path).is_some());
    }
}
