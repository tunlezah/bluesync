//! Default-sink routing (pactl, with a wpctl fallback) and module unload
//! (AUD-022/029).
use crate::audio::pw::parse::find_wpctl_node_id;
use crate::sys::command::CommandRunner;

/// Which mechanism set the default sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultSink {
    Pactl,
    Wpctl(u32),
}

/// Route the default sink to `sink_name`. Tries `pactl set-default-sink`; on
/// failure (non-zero or spawn error) falls back to resolving the node id from
/// `wpctl status` and `wpctl set-default <id>`.
pub fn set_default_sink<R: CommandRunner>(
    runner: &R,
    sink_name: &str,
) -> Result<DefaultSink, String> {
    if let Ok(out) = runner.run("pactl", &["set-default-sink", sink_name]) {
        if out.success() {
            return Ok(DefaultSink::Pactl);
        }
    }

    let status = runner
        .run("wpctl", &["status"])
        .map_err(|e| format!("wpctl status failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "cannot set default sink {sink_name}: pactl failed and `wpctl status` failed"
        ));
    }
    let id = find_wpctl_node_id(&status.stdout, sink_name)
        .ok_or_else(|| format!("{sink_name} not found in `wpctl status`"))?;
    let id_str = id.to_string();
    let set = runner
        .run("wpctl", &["set-default", &id_str])
        .map_err(|e| format!("wpctl set-default failed: {e}"))?;
    if set.success() {
        Ok(DefaultSink::Wpctl(id))
    } else {
        Err(format!(
            "`wpctl set-default {id}` failed: {}",
            set.stderr.trim()
        ))
    }
}

/// Unload a PulseAudio/pipewire-pulse module by id (`pactl unload-module`).
pub fn unload_module<R: CommandRunner>(runner: &R, module_id: u32) -> Result<(), String> {
    let id_str = module_id.to_string();
    let out = runner
        .run("pactl", &["unload-module", &id_str])
        .map_err(|e| format!("failed to run pactl: {e}"))?;
    if out.success() {
        Ok(())
    } else {
        Err(format!(
            "unload-module {module_id} failed: {}",
            out.stderr.trim()
        ))
    }
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
    fn fail() -> CommandOutput {
        CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "nope".into(),
        }
    }

    #[test]
    fn sets_default_via_pactl_when_it_succeeds() {
        let runner = FakeCommandRunner::new().on("pactl", &["set-default-sink"], ok(""));
        assert_eq!(
            set_default_sink(&runner, "soundsync-capture").unwrap(),
            DefaultSink::Pactl
        );
        assert!(!runner.ran("wpctl")); // no fallback needed
    }

    #[test]
    fn falls_back_to_wpctl_when_pactl_fails() {
        let status = " │      40. soundsync-capture [vol: 1.00]\n";
        let runner = FakeCommandRunner::new()
            .on("pactl", &["set-default-sink"], fail())
            .on("wpctl", &["status"], ok(status))
            .on("wpctl", &["set-default"], ok(""));
        assert_eq!(
            set_default_sink(&runner, "soundsync-capture").unwrap(),
            DefaultSink::Wpctl(40)
        );
    }

    #[test]
    fn errors_when_wpctl_cannot_find_node() {
        let runner = FakeCommandRunner::new()
            .on("pactl", &["set-default-sink"], fail())
            .on("wpctl", &["status"], ok("no matching node here\n"));
        assert!(set_default_sink(&runner, "soundsync-capture").is_err());
    }

    #[test]
    fn unload_module_ok_and_err() {
        let ok_runner = FakeCommandRunner::new().on("pactl", &["unload-module"], ok(""));
        assert!(unload_module(&ok_runner, 7).is_ok());
        let bad_runner = FakeCommandRunner::new().on("pactl", &["unload-module"], fail());
        assert!(unload_module(&bad_runner, 7).is_err());
    }
}
