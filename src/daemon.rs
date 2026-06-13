//! The daemon: assemble the units into a running pipeline (see
//! research/daemon-design.md). User-level only — no root ops (WP-config apply is
//! install-time). EQ is off for now, so the default sink is the null sink.
use crate::audio::capture::run_capture;
use crate::audio::pw::capture_cmd::CaptureTool;
use crate::audio::pw::capture_cmd::{build_capture_command, CaptureSource};
use crate::audio::pw::events::{run_event_source, GraphEvent};
use crate::audio::pw::null_sink::{ensure_null_sink, sweep_duplicate_null_sinks};
use crate::audio::pw::quantum::probe_quantum;
use crate::audio::pw::routing::set_default_sink;
use crate::sys::command::CommandRunner;
use crate::sys::process::ProcessSpawner;
use crate::sys::supervisor::{supervise, wait_for_shutdown, RestartPolicy};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::Duration;

/// Commands sent to the daemon from the web layer.
#[derive(Debug)]
pub enum DaemonCommand {
    /// Re-assert the current routing (recreate null sink + set default sink).
    ReassertRouting,
    /// Play a brief audible test tone so the user can confirm audio routing.
    Identify,
}

/// The capture null-sink name + description (AUD-021).
pub const NULL_SINK_NAME: &str = "soundsync-capture";
pub const NULL_SINK_DESC: &str = "SoundSync-Capture";

/// The EQ sink name (Audio/Sink side of the filter-chain). Correction 2.
pub const EQ_SINK_NAME: &str = "effect_input.soundsync-eq";

/// Desired routing state published by the EQ controller and consumed by the
/// reconcile loop. The reconcile loop re-asserts `default_sink` on every graph
/// event so transient PipeWire disturbances are healed automatically (AUD-022).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredRoute {
    /// The PipeWire sink name to use as the system default sink.
    pub default_sink: String,
}

impl Default for DesiredRoute {
    fn default() -> Self {
        // EQ starts disabled → route directly to null sink.
        Self {
            default_sink: NULL_SINK_NAME.to_string(),
        }
    }
}

/// Detect the available capture tool: prefer `parec`, fall back to `pw-cat`.
pub fn detect_capture_tool<R: CommandRunner>(runner: &R) -> Option<CaptureTool> {
    let present = |prog: &str| {
        runner
            .run("which", &[prog])
            .map(|o| o.success())
            .unwrap_or(false)
    };
    if present("parec") {
        Some(CaptureTool::Parec)
    } else if present("pw-cat") {
        Some(CaptureTool::PwCat)
    } else {
        None
    }
}

/// One-shot user-level bring-up of the capture graph: ensure the null sink
/// exists, then make it the desired default sink (EQ-aware via `DesiredRoute`).
///
/// Async (W0.1): the null-sink retry/backoff now yields to the executor, so a
/// caller can `select!` this against shutdown rather than blocking a runtime
/// thread until `pactl` succeeds (W0.2).
pub async fn bring_up<R: CommandRunner>(runner: &R, route: &DesiredRoute) -> Result<(), String> {
    ensure_null_sink(runner, NULL_SINK_NAME, NULL_SINK_DESC).await?;
    set_default_sink(runner, &route.default_sink)?;
    Ok(())
}

/// Run the daemon: detect the capture tool, bring up the graph, run the
/// supervised capture unit, and block until `shutdown` is signalled. The
/// `runner` is used only for the synchronous setup; only the spawner
/// (cloned `Arc`) crosses into the supervised task.
pub async fn run_daemon<R, Sp>(
    runner: &R,
    spawner: Arc<Sp>,
    pcm_bus: broadcast::Sender<Vec<f32>>,
    route_rx: watch::Receiver<DesiredRoute>,
    mut daemon_rx: mpsc::Receiver<DaemonCommand>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), String>
where
    R: CommandRunner,
    Sp: ProcessSpawner + Send + Sync + 'static,
{
    let tool = detect_capture_tool(runner)
        .ok_or_else(|| "no capture tool found (need parec or pw-cat)".to_string())?;
    let latency = probe_quantum(runner).capture_latency_ms();

    // Bring-up with the current desired route (EQ off at startup). Race it
    // against shutdown so a SIGTERM during a slow `pactl` retry/backoff
    // (e.g. pipewire-pulse not ready) returns promptly instead of hanging
    // until the null-sink attempts are exhausted (W0.2).
    //
    // At startup only, ensure the null sink FIRST (establishing the canonical
    // module id), then sweep away any leftover duplicate `soundsync-capture`
    // null-sink modules from a previous run before routing. Startup-only by
    // design: folding the sweep into `bring_up`/reconcile risks an unload →
    // sink-remove → reconcile → bring_up → sweep event storm (W2.1 / H5).
    let initial_route = route_rx.borrow().clone();
    {
        let mut sd = shutdown.clone();
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut sd) => return Ok(()),
            r = ensure_null_sink(runner, NULL_SINK_NAME, NULL_SINK_DESC) => {
                let null_sink = r?;
                // Best-effort: a stuck duplicate must never abort startup.
                if let Some(canonical) = null_sink.module_id {
                    sweep_duplicate_null_sinks(runner, NULL_SINK_NAME, canonical);
                }
                set_default_sink(runner, &initial_route.default_sink)?;
            }
        }
    }

    let cmd = build_capture_command(
        tool,
        &CaptureSource::NullSinkMonitor(NULL_SINK_NAME.to_string()),
        latency,
    );

    let tx = pcm_bus;

    // Supervised capture on its own task; `tx` (PCM bus) moves into it.
    let capture = {
        let spawner = spawner.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            let factory = move || {
                let spawner = spawner.clone();
                let cmd = cmd.clone();
                let tx = tx.clone();
                async move { run_capture(&*spawner, &cmd, &tx).await }
            };
            supervise("capture", factory, RestartPolicy::default(), sd).await
        })
    };

    // Supervised event source feeding the reconcile loop.
    let (ev_tx, ev_rx) = mpsc::channel::<GraphEvent>(64);
    let events = {
        let spawner = spawner.clone();
        let sd = shutdown.clone();
        tokio::spawn(async move {
            let factory = move || {
                let spawner = spawner.clone();
                let ev_tx = ev_tx.clone();
                async move { run_event_source(&*spawner, ev_tx).await }
            };
            supervise("events", factory, RestartPolicy::default(), sd).await
        })
    };

    // Daemon command handler: runs concurrently with the reconcile loop.
    // Borrows route_rx (for ReassertRouting) and runner/sleeper.
    // Identify spawns a fire-and-forget test-tone process (non-blocking).
    let daemon_cmd = {
        let sd = shutdown.clone();
        let route_rx2 = route_rx.clone();
        async move {
            let mut shutdown2 = sd;
            loop {
                tokio::select! {
                    biased;
                    _ = wait_for_shutdown(&mut shutdown2) => return,
                    cmd = daemon_rx.recv() => {
                        let Some(cmd) = cmd else { return };
                        match cmd {
                            DaemonCommand::ReassertRouting => {
                                let route = route_rx2.borrow().clone();
                                // Race the (async) bring-up against shutdown so an
                                // admin restart in flight can't block the daemon's
                                // exit (W0.2).
                                tokio::select! {
                                    biased;
                                    _ = wait_for_shutdown(&mut shutdown2) => return,
                                    r = bring_up(runner, &route) => {
                                        if let Err(e) = r {
                                            eprintln!("soundsync: admin restart failed: {e}");
                                        }
                                    }
                                }
                            }
                            DaemonCommand::Identify => {
                                // Spawn a brief audible test tone (880 Hz, 1 s) to the
                                // capture sink so listeners hear it.  Best-effort; non-blocking.
                                // The spawned task owns `child` and awaits it so the full
                                // 1-second tone plays; kill_on_drop still reaps it if the
                                // task is cancelled before completion.
                                match tokio::process::Command::new("sh")
                                    .args([
                                        "-c",
                                        "ffmpeg -f lavfi -i sine=frequency=880:duration=1 \
                                         -ac 2 -ar 48000 -f s16le - 2>/dev/null | \
                                         pw-cat --playback --rate=48000 --channels=2 \
                                         --format=s16le --target=soundsync-capture - 2>/dev/null",
                                    ])
                                    .kill_on_drop(true)
                                    .spawn()
                                {
                                    Ok(mut child) => {
                                        tokio::spawn(async move {
                                            let _ = child.wait().await;
                                        });
                                    }
                                    Err(e) => eprintln!("soundsync: identify tone failed to spawn: {e}"),
                                }
                            }
                        }
                    }
                }
            }
        }
    };

    // Reconcile inline until shutdown (borrows runner — no spawn).
    tokio::join!(
        reconcile_loop(
            runner,
            ev_rx,
            route_rx,
            RECONCILE_DEBOUNCE,
            shutdown.clone(),
        ),
        daemon_cmd,
    );

    // Once the reconcile loop has returned (shutdown, or the event source ended
    // unexpectedly), stop the supervised tasks so the daemon can never hang
    // waiting on a task that didn't observe shutdown. `abort` is a no-op if the
    // task already finished on its own; cancelling a live capture drops its
    // child, which `kill_on_drop` reaps.
    capture.abort();
    events.abort();
    let _ = capture.await;
    let _ = events.await;
    Ok(())
}

/// Debounce window: collapse a burst of graph events into one reconcile.
const RECONCILE_DEBOUNCE: Duration = Duration::from_millis(200);

/// React to graph changes: on a (debounced) batch of `GraphEvent`s, idempotently
/// re-assert the desired state via `bring_up` (recreate the null sink if it was
/// removed, re-set the default sink if it drifted). Reads the current
/// `DesiredRoute` from the watch channel on each reconcile so EQ enable/disable
/// is always reflected (AUD-022). Returns on shutdown or when the event source's
/// sender is gone.
pub async fn reconcile_loop<R>(
    runner: &R,
    mut rx: mpsc::Receiver<GraphEvent>,
    route_rx: watch::Receiver<DesiredRoute>,
    debounce: Duration,
    mut shutdown: watch::Receiver<bool>,
) where
    R: CommandRunner,
{
    loop {
        // Wait for the first event (or shutdown / source-gone).
        let first = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            ev = rx.recv() => ev,
        };
        if first.is_none() {
            return; // event source gone
        }

        // Debounce: drain further events until the window elapses.
        let sleep = tokio::time::sleep(debounce);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => return,
                _ = &mut sleep => break,
                ev = rx.recv() => {
                    if ev.is_none() {
                        break; // source gone — reconcile once more, then exit on next recv
                    }
                }
            }
        }

        // Idempotent re-assert of desired state. Read current route on every
        // reconcile so EQ enable/disable is always respected (AUD-022). Race the
        // async bring-up against shutdown so a slow null-sink retry can't stall
        // the loop's exit (W0.2).
        let route = route_rx.borrow().clone();
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            r = bring_up(runner, &route) => {
                if let Err(e) = r {
                    eprintln!("soundsync: reconcile failed (will retry on next event): {e}");
                }
            }
        }
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

    #[test]
    fn detects_parec_first_then_pwcat() {
        let parec = FakeCommandRunner::new().on("which", &["parec"], ok(""));
        assert_eq!(detect_capture_tool(&parec), Some(CaptureTool::Parec));

        // parec absent (which returns 127), pw-cat present
        let pwcat = FakeCommandRunner::new().on("which", &["pw-cat"], ok(""));
        assert_eq!(detect_capture_tool(&pwcat), Some(CaptureTool::PwCat));

        // neither present
        assert_eq!(detect_capture_tool(&FakeCommandRunner::new()), None);
    }

    #[tokio::test(start_paused = true)]
    async fn bring_up_ensures_sink_then_routes() {
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok("")) // no existing null sink
            .on("pactl", &["load-module"], ok("42"))
            .on(
                "pactl",
                &["short", "sinks"],
                ok("42\tsoundsync-capture\tPipeWire\tfloat32le 2ch 48000Hz\tSUSPENDED\n"),
            )
            .on("pactl", &["set-default-sink"], ok(""));
        bring_up(&runner, &DesiredRoute::default()).await.unwrap();

        let calls = runner.calls();
        let issued = |sub: &str| {
            calls
                .iter()
                .any(|(p, a)| p == "pactl" && a.iter().any(|x| x == sub))
        };
        assert!(issued("load-module")); // null sink created
        assert!(issued("set-default-sink")); // routed
    }

    #[tokio::test(start_paused = true)]
    async fn bring_up_routes_to_eq_sink_when_desired() {
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok("Module #7\n\tName: module-null-sink\n\tArgument: sink_name=soundsync-capture\n"))
            .on("pactl", &["set-default-sink"], ok(""));
        let route = DesiredRoute {
            default_sink: EQ_SINK_NAME.to_string(),
        };
        bring_up(&runner, &route).await.unwrap();

        let calls = runner.calls();
        let sink_calls: Vec<_> = calls
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "set-default-sink"))
            .collect();
        assert_eq!(sink_calls.len(), 1);
        // The EQ sink name appears in the set-default-sink args.
        assert!(sink_calls[0].1.contains(&EQ_SINK_NAME.to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn bring_up_fails_if_null_sink_cannot_be_created() {
        // load-module fails and the sink never appears -> ensure_null_sink errs.
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok(""))
            .on(
                "pactl",
                &["load-module"],
                CommandOutput {
                    status: 1,
                    stdout: String::new(),
                    stderr: "boom".into(),
                },
            )
            .on("pactl", &["short", "sinks"], ok("other\n"))
            .on("pactl", &["set-default-sink"], ok(""));
        assert!(bring_up(&runner, &DesiredRoute::default()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn reconcile_loop_debounces_burst_into_one_reassert() {
        use crate::audio::pw::events::GraphEvent;
        use tokio::sync::mpsc;
        use tokio::time::Duration;

        // Null sink already exists -> reconcile reuses it (no load-module) and re-sets default.
        let runner = FakeCommandRunner::new()
            .on("pactl", &["list", "modules"], ok("Module #7\n\tName: module-null-sink\n\tArgument: sink_name=soundsync-capture\n"))
            .on("pactl", &["set-default-sink"], ok(""));
        let (ev_tx, ev_rx) = mpsc::channel(8);
        let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let (_route_tx, route_rx) = tokio::sync::watch::channel(DesiredRoute::default());
        let debounce = Duration::from_millis(200);

        let driver = async {
            // Burst of 3 events within the debounce window.
            ev_tx.send(GraphEvent::Sink).await.unwrap();
            ev_tx.send(GraphEvent::Server).await.unwrap();
            ev_tx.send(GraphEvent::Sink).await.unwrap();
            // Wait well past the debounce so exactly one reconcile fires, then stop.
            tokio::time::sleep(debounce * 3).await;
            sd_tx.send(true).unwrap();
        };
        tokio::join!(
            reconcile_loop(&runner, ev_rx, route_rx, debounce, sd_rx),
            driver,
        );

        // Exactly one re-assert (set-default-sink) for the whole burst.
        let n = runner
            .calls()
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "set-default-sink"))
            .count();
        assert_eq!(n, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn reconcile_loop_returns_on_shutdown_with_no_events() {
        use tokio::sync::mpsc;
        use tokio::time::Duration;
        let runner = FakeCommandRunner::new();
        let (_ev_tx, ev_rx) = mpsc::channel(8);
        let (_sd_tx, sd_rx) = tokio::sync::watch::channel(true); // already shut down
        let (_route_tx, route_rx) = tokio::sync::watch::channel(DesiredRoute::default());
        reconcile_loop(&runner, ev_rx, route_rx, Duration::from_millis(200), sd_rx).await;
        assert!(runner.calls().is_empty()); // never reconciled
    }

    #[tokio::test(start_paused = true)]
    async fn run_daemon_brings_up_then_stops_on_shutdown() {
        use crate::sys::testing::FakeProcessSpawner;
        use std::sync::Arc;
        use tokio::sync::watch;

        let runner = FakeCommandRunner::new()
            .on("which", &["parec"], ok(""))
            .on("pw-metadata", &["settings"], ok("update: id:0 key:'clock.quantum' value:'1024' type:''\nupdate: id:0 key:'clock.rate' value:'48000' type:''\n"))
            .on("pactl", &["list", "modules"], ok(""))
            .on("pactl", &["load-module"], ok("42"))
            .on("pactl", &["short", "sinks"], ok("42\tsoundsync-capture\tPipeWire\tfloat32le 2ch 48000Hz\tSUSPENDED\n"))
            .on("pactl", &["set-default-sink"], ok(""));
        let spawner = Arc::new(FakeProcessSpawner::new(Some(0)));
        // Start running, then signal shutdown after bring-up so the daemon
        // both performs the bring-up AND returns promptly (it no longer brings
        // up if shutdown is already set when it starts — W0.2).
        let (sd_tx, rx) = watch::channel(false);
        let (_route_tx, route_rx) = watch::channel(DesiredRoute::default());
        let (_daemon_tx, daemon_rx) = mpsc::channel::<DaemonCommand>(4);
        let pcm_bus = crate::audio::capture::new_pcm_bus();

        let driver = async move {
            // Let startup bring-up complete, then request shutdown.
            tokio::time::sleep(Duration::from_millis(50)).await;
            sd_tx.send(true).unwrap();
        };
        let (daemon_result, _) = tokio::join!(
            run_daemon(&runner, spawner, pcm_bus, route_rx, daemon_rx, rx),
            driver,
        );
        daemon_result.unwrap();

        // Bring-up effects happened before the daemon awaited shutdown.
        let calls = runner.calls();
        let issued = |sub: &str| {
            calls
                .iter()
                .any(|(p, a)| p == "pactl" && a.iter().any(|x| x == sub))
        };
        assert!(issued("load-module"));
        assert!(issued("set-default-sink"));
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_reassert_routing_command_triggers_bring_up() {
        use crate::sys::testing::FakeProcessSpawner;
        // When a ReassertRouting command arrives the daemon calls bring_up.
        // Use a shutdown-pre-signalled watch so run_daemon exits immediately
        // after the daemon_cmd task sees the command.
        let runner = FakeCommandRunner::new()
            .on("which", &["parec"], ok(""))
            .on("pw-metadata", &["settings"], ok("update: id:0 key:'clock.quantum' value:'1024' type:''\nupdate: id:0 key:'clock.rate' value:'48000' type:''\n"))
            .on("pactl", &["list", "modules"], ok("Module #7\n\tName: module-null-sink\n\tArgument: sink_name=soundsync-capture\n"))
            .on("pactl", &["set-default-sink"], ok(""));
        let spawner = Arc::new(FakeProcessSpawner::new(Some(0)));
        let (sd_tx, sd_rx) = watch::channel(false);
        let (_route_tx, route_rx) = watch::channel(DesiredRoute::default());
        let (daemon_tx, daemon_rx) = mpsc::channel::<DaemonCommand>(4);
        let pcm_bus = crate::audio::capture::new_pcm_bus();

        let driver = async move {
            daemon_tx
                .send(DaemonCommand::ReassertRouting)
                .await
                .unwrap();
            // Give the daemon_cmd task time to process the command.
            tokio::time::sleep(Duration::from_millis(50)).await;
            sd_tx.send(true).unwrap();
        };

        let (daemon_result, _) = tokio::join!(
            run_daemon(&runner, spawner, pcm_bus, route_rx, daemon_rx, sd_rx),
            driver,
        );
        daemon_result.unwrap();

        // bring_up was called: set-default-sink was issued.
        let calls = runner.calls();
        let n = calls
            .iter()
            .filter(|(p, a)| p == "pactl" && a.iter().any(|x| x == "set-default-sink"))
            .count();
        // At least 1 (the initial bring_up) + 1 from ReassertRouting.
        assert!(
            n >= 2,
            "expected startup + reassert bring_up (>=2 set-default-sink calls), got {n}"
        );
    }
}
