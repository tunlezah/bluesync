//! Live EQ control via `pw-cli set-param` (§8b LIVE-VERIFIED corrections applied).
//!
//! Correction 2 (UB-6): the `eq_band_N:Gain` Props live on the node named
//! `effect_input.soundsync-eq` (the Audio/Sink), NOT `soundsync-eq`.
use crate::sys::command::CommandRunner;
use std::time::Duration;
use tokio::time::Instant;

/// The PipeWire node name whose `eq_band_N:Gain` Props are updated live.
/// This is the Audio/Sink side of the filter-chain (Correction 2).
pub const EQ_NODE_NAME: &str = "effect_input.soundsync-eq";

/// Parse a `pw-dump` JSON array and return the PipeWire object `id` of the node
/// whose `info.props["node.name"]` equals [`EQ_NODE_NAME`].
///
/// Returns `None` on any parse error or if the node is absent.
pub fn find_eq_node_id(pw_dump_json: &str) -> Option<u32> {
    let objects: serde_json::Value = serde_json::from_str(pw_dump_json).ok()?;
    let arr = objects.as_array()?;
    for obj in arr {
        let node_name = obj
            .get("info")
            .and_then(|i| i.get("props"))
            .and_then(|p| p.get("node.name"))
            .and_then(|v| v.as_str());
        if node_name == Some(EQ_NODE_NAME) {
            let id = obj.get("id")?.as_u64()? as u32;
            return Some(id);
        }
    }
    None
}

/// Run `pw-dump`, parse it via [`find_eq_node_id`], and return the node id.
pub async fn probe_eq_node_id<R: CommandRunner>(runner: &R) -> Option<u32> {
    let out = runner.run("pw-dump", &[]).ok()?;
    if !out.success() {
        return None;
    }
    find_eq_node_id(&out.stdout)
}

/// Build the `Props` param string for a single `pw-cli set-param` call that
/// sets all 10 band gains atomically (one PipeWire graph cycle).
///
/// Format: `{ params = [ "eq_band_0:Gain" <g0> ... "eq_band_9:Gain" <g9> ] }`
pub fn build_gain_params(gains: &[f32]) -> String {
    let mut s = String::from("{ params = [");
    for (i, g) in gains.iter().enumerate() {
        s.push_str(&format!(" \"eq_band_{i}:Gain\" {g:.1}"));
    }
    s.push_str(" ] }");
    s
}

/// Set all EQ band gains on the running filter-chain node via `pw-cli set-param`.
/// All bands are updated in a single atomic Props call (one graph cycle — no glitch).
pub async fn set_eq_gains<R: CommandRunner>(
    runner: &R,
    node_id: u32,
    gains: &[f32],
) -> Result<(), String> {
    let id_str = node_id.to_string();
    let params = build_gain_params(gains);
    let out = runner
        .run("pw-cli", &["set-param", &id_str, "Props", &params])
        .map_err(|e| format!("pw-cli set-param failed to spawn: {e}"))?;
    if out.success() {
        Ok(())
    } else {
        Err(format!(
            "pw-cli set-param {node_id} Props failed: {}",
            out.stderr.trim()
        ))
    }
}

/// Poll for the EQ node to appear in the PipeWire graph, checking every 50 ms.
/// Returns the node id once found, or `None` on timeout.
///
/// Replaces the legacy blind 500 ms sleep (AUD-024) — the node typically appears
/// within 200–400 ms on PipeWire 1.0.5.
pub async fn wait_for_eq_node<R: CommandRunner>(runner: &R, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(id) = probe_eq_node_id(runner).await {
            return Some(id);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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

    const SAMPLE_PW_DUMP: &str = r#"[
  {
    "id": 7,
    "type": "PipeWire:Interface:Node",
    "info": {
      "props": {
        "node.name": "other-node"
      }
    }
  },
  {
    "id": 42,
    "type": "PipeWire:Interface:Node",
    "info": {
      "props": {
        "node.name": "effect_input.soundsync-eq",
        "media.class": "Audio/Sink"
      }
    }
  },
  {
    "id": 43,
    "type": "PipeWire:Interface:Node",
    "info": {
      "props": {
        "node.name": "effect_output.soundsync-eq"
      }
    }
  }
]"#;

    #[test]
    fn find_eq_node_id_present() {
        assert_eq!(find_eq_node_id(SAMPLE_PW_DUMP), Some(42));
    }

    #[test]
    fn find_eq_node_id_absent() {
        let json = r#"[{"id":1,"info":{"props":{"node.name":"unrelated"}}}]"#;
        assert_eq!(find_eq_node_id(json), None);
    }

    #[test]
    fn find_eq_node_id_malformed_json() {
        assert_eq!(find_eq_node_id("not json"), None);
        assert_eq!(find_eq_node_id("{}"), None); // object not array
        assert_eq!(find_eq_node_id("[]"), None); // empty
    }

    #[test]
    fn build_gain_params_exact_format() {
        let gains = [1.0_f32, -2.5, 0.0, 3.0, -12.0, 0.5, 6.0, -1.0, 2.0, 0.0];
        let s = build_gain_params(&gains);
        assert!(s.starts_with("{ params = ["));
        assert!(s.ends_with("] }"));
        assert!(s.contains("\"eq_band_0:Gain\" 1.0"));
        assert!(s.contains("\"eq_band_1:Gain\" -2.5"));
        assert!(s.contains("\"eq_band_9:Gain\" 0.0"));
    }

    #[test]
    fn build_gain_params_single_band() {
        let gains = [6.0_f32];
        let s = build_gain_params(&gains);
        assert_eq!(s, "{ params = [ \"eq_band_0:Gain\" 6.0 ] }");
    }

    #[tokio::test]
    async fn probe_eq_node_id_parses_pw_dump() {
        let runner = FakeCommandRunner::new().on("pw-dump", &[], ok(SAMPLE_PW_DUMP));
        assert_eq!(probe_eq_node_id(&runner).await, Some(42));
    }

    #[tokio::test]
    async fn probe_eq_node_id_returns_none_on_failure() {
        let runner = FakeCommandRunner::new().on("pw-dump", &[], fail());
        assert_eq!(probe_eq_node_id(&runner).await, None);
    }

    #[tokio::test]
    async fn set_eq_gains_issues_correct_argv() {
        let gains = [0.0_f32; 10];
        let expected_params = build_gain_params(&gains);
        let runner = FakeCommandRunner::new().on("pw-cli", &["set-param"], ok(""));
        set_eq_gains(&runner, 42, &gains).await.unwrap();

        let calls = runner.calls();
        let (prog, args) = &calls[0];
        assert_eq!(prog, "pw-cli");
        assert_eq!(args[0], "set-param");
        assert_eq!(args[1], "42");
        assert_eq!(args[2], "Props");
        assert_eq!(args[3], expected_params);
    }

    #[tokio::test]
    async fn set_eq_gains_errors_on_failure() {
        let runner = FakeCommandRunner::new().on("pw-cli", &["set-param"], fail());
        let result = set_eq_gains(&runner, 42, &[0.0_f32; 10]).await;
        assert!(result.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_eq_node_returns_id_when_found() {
        let runner = FakeCommandRunner::new().on("pw-dump", &[], ok(SAMPLE_PW_DUMP));
        let result = wait_for_eq_node(&runner, Duration::from_millis(500)).await;
        assert_eq!(result, Some(42));
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_eq_node_returns_none_on_timeout() {
        // pw-dump always returns empty array -> node never appears
        let runner = FakeCommandRunner::new().on("pw-dump", &[], ok("[]"));
        let result = wait_for_eq_node(&runner, Duration::from_millis(100)).await;
        assert_eq!(result, None);
    }
}
