//! `OutputController` task — OUT-4b Chromecast routing (multi-output-design.md).
//!
//! OUT-4b adds on top of OUT-3:
//! - `run_output_controller` now accepts `lan_ip: String` and `bind_port: u16`
//!   so it can compute the Chromecast LOAD URL at routing time.
//! - `cast_handle: Option<cast::client::CastHandle>` tracked alongside the loopback.
//! - **Combined teardown**: stops BOTH any active loopback AND any active cast session.
//!   This enforces single-active across ALL output kinds (soundcard / AirPlay /
//!   Chromecast). Starting a new kind tears down the old one first.
//! - `Select{Chromecast, id}`: teardown → find device → `start_cast(addr, port,
//!   stream_url(lan_ip, bind_port))` → store `cast_handle` + set `active`.
//!   No loopback is loaded for Chromecast (it pulls the HTTP stream directly).
//! - Shutdown also stops any active cast session.
//!
//! OUT-3 behaviour (unchanged):
//! - Loads `module-raop-discover` on startup so PipeWire RAOP sinks appear.
//!   Stores the module id in `raop_discover_module_id: Option<u32>` (best-effort
//!   — AirPlay unavailable on failure, no crash).
//! - AirPlay `Select` routes identically to Soundcard: teardown current →
//!   `build_loopback_args(sink_id)` → load-module → store loopback id → set active.
//!   Shared via the private `route_to_sink` helper.
//! - On exit (shutdown or rx-closed): unloads the raop-discover module after the
//!   loopback teardown so RAOP sinks don't linger in PipeWire.
//!
//! OUT-2 behaviour (unchanged):
//! - Owns `Arc<R>` where `R: CommandRunner + Send + Sync + 'static` so that every
//!   blocking `runner.run(...)` call is dispatched through
//!   `tokio::task::spawn_blocking`, keeping the async runtime unblocked.
//! - Tracks `active: Option<OutputDevice>` and `loopback_module_id: Option<u32>`.
//! - `OutputCommand::None`: teardown + clear active + publish.
//! - Discovery refresh (~10 s) and the initial probe also run via
//!   `spawn_blocking` (fixes the OUT-1 sync-block-on-async-runtime concern).
//! - Biased `select!`: shutdown takes priority.

use crate::cast::client::{start_cast, stream_url, CastSession};
use crate::output::discovery::{parse_module_id_by_name, probe_outputs};
use crate::output::loopback::{build_loopback_args, parse_loopback_module_id};
use crate::output::{AvailableOutputs, OutputCommand, OutputDevice, OutputKind};
use crate::state::{AppStateHandle, CastHealth, OutputState};
use crate::sys::command::CommandRunner;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Discovery refresh interval (best-effort; biased toward shutdown).
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// Find a device by kind + id in the available outputs.
fn find_device(available: &AvailableOutputs, kind: &OutputKind, id: &str) -> Option<OutputDevice> {
    let list = match kind {
        OutputKind::Soundcard => &available.soundcard,
        OutputKind::Airplay => &available.airplay,
        OutputKind::Chromecast => &available.chromecast,
    };
    list.iter().find(|d| d.id == id).cloned()
}

/// Run the output controller task.
///
/// Drives discovery refreshes and processes `OutputCommand` messages.
///
/// `runner` is `Arc<R>` so `R` can be moved into `spawn_blocking` closures
/// (which require `'static + Send`).  `Arc` satisfies the `'static` bound and
/// `Arc<R>: Send` when `R: Send + Sync`.
///
/// `lan_ip` is the server's LAN IPv4 (from `hostname -I` via `server_lan_ip`);
/// `bind_port` is the HTTP server port.  These are used to build the Chromecast
/// LOAD URL.  If `lan_ip` is empty, Chromecast routing will still tear down
/// any existing session but will skip starting a new cast (the URL would be
/// unusable) and log an error — `active` is left as `None`.
///
/// # Shutdown
/// Exits when `shutdown` becomes `true` or when `rx` is closed.  The shutdown
/// check is biased: a pending shutdown wakes the select immediately.
pub async fn run_output_controller<R>(
    mut rx: mpsc::Receiver<OutputCommand>,
    runner: Arc<R>,
    state: AppStateHandle,
    mut shutdown: watch::Receiver<bool>,
    lan_ip: String,
    bind_port: u16,
) where
    R: CommandRunner + Send + Sync + 'static,
{
    // ── Load module-raop-discover so RAOP sinks appear in PipeWire ──────────
    //
    // Best-effort: on failure AirPlay sinks simply won't be discovered.
    // The module id is stored so we can unload it on shutdown.
    let mut raop_discover_module_id: Option<u32> = {
        let r = Arc::clone(&runner);
        match tokio::task::spawn_blocking(move || {
            r.run("pactl", &["load-module", "module-raop-discover"])
        })
        .await
        {
            Ok(Ok(out)) if out.success() => {
                let mid = out.stdout.trim().parse::<u32>().ok();
                if mid.is_none() {
                    eprintln!(
                        "soundsync/output: module-raop-discover loaded but returned \
                         unparseable module id {:?} — cannot unload on shutdown",
                        out.stdout.trim()
                    );
                }
                mid
            }
            Ok(Ok(_out)) => {
                // load-module failed (non-zero status) — the most likely cause is
                // "Failure: Entity exists" from a prior crashed daemon that left the
                // module loaded.  Query the existing module id rather than giving up.
                let r2 = Arc::clone(&runner);
                let list_result = tokio::task::spawn_blocking(move || {
                    r2.run("pactl", &["list", "short", "modules"])
                })
                .await;
                match list_result {
                    Ok(Ok(list_out)) if list_out.success() => {
                        match parse_module_id_by_name(&list_out.stdout, "module-raop-discover") {
                            Some(mid) => {
                                eprintln!(
                                    "soundsync/output: module-raop-discover already loaded \
                                     (id {mid}) — AirPlay available"
                                );
                                Some(mid)
                            }
                            None => {
                                eprintln!(
                                    "soundsync/output: pactl load-module module-raop-discover \
                                     failed and module not found in list — AirPlay unavailable"
                                );
                                None
                            }
                        }
                    }
                    Ok(Ok(list_out)) => {
                        eprintln!(
                            "soundsync/output: pactl list short modules failed (status {}) \
                             while recovering raop-discover — AirPlay unavailable",
                            list_out.status
                        );
                        None
                    }
                    Ok(Err(e)) => {
                        eprintln!(
                            "soundsync/output: failed to spawn pactl list short modules \
                             while recovering raop-discover: {e} — AirPlay unavailable"
                        );
                        None
                    }
                    Err(e) => {
                        eprintln!(
                            "soundsync/output: spawn_blocking error listing modules \
                             while recovering raop-discover: {e} — AirPlay unavailable"
                        );
                        None
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!(
                    "soundsync/output: failed to spawn pactl for module-raop-discover: {e} \
                     — AirPlay unavailable"
                );
                None
            }
            Err(e) => {
                eprintln!(
                    "soundsync/output: spawn_blocking error loading module-raop-discover: {e} \
                     — AirPlay unavailable"
                );
                None
            }
        }
    };

    // ── Initial discovery (spawn_blocking so we don't block the runtime) ───
    let r = Arc::clone(&runner);
    let mut available: AvailableOutputs =
        match tokio::task::spawn_blocking(move || probe_outputs(&*r)).await {
            Ok(a) => a,
            Err(e) => {
                eprintln!("soundsync/output: initial probe spawn_blocking error: {e}");
                AvailableOutputs::default()
            }
        };
    let mut active: Option<OutputDevice> = None;
    let mut loopback_module_id: Option<u32> = None;
    // OUT-4b: tracks an active CASTV2 session (mutually exclusive with loopback).
    // NF-8: the session carries the task's JoinHandle so we can detect it dying.
    let mut cast_session: Option<CastSession> = None;
    // NF-8: health of the active cast session, surfaced to the UI.
    let mut cast_health: Option<CastHealth> = None;

    // Publish initial state.
    state
        .set_output(OutputState {
            active: active.clone(),
            available: available.clone(),
            cast_health: cast_health.clone(),
        })
        .await;

    let mut refresh_interval = tokio::time::interval(REFRESH_INTERVAL);
    // Skip the immediate first tick — we already probed above.
    refresh_interval.tick().await;

    loop {
        tokio::select! {
            biased;

            // Shutdown takes priority.
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }

            // Incoming command from the HTTP handler.
            cmd = rx.recv() => {
                match cmd {
                    None => break, // sender dropped

                    Some(OutputCommand::None) => {
                        // Tear down any current loopback AND cast session, clear active.
                        teardown(&runner, &mut loopback_module_id, &mut cast_session).await;
                        active = None;
                        cast_health = None;
                        state.set_output(OutputState {
                            active: active.clone(),
                            available: available.clone(),
                            cast_health: cast_health.clone(),
                        }).await;
                    }

                    Some(OutputCommand::Select { kind: OutputKind::Soundcard, id }) => {
                        // Tear down current (loopback + cast), load loopback to the chosen
                        // soundcard sink, then set active.  Shared logic with AirPlay via
                        // route_to_sink.
                        teardown(&runner, &mut loopback_module_id, &mut cast_session).await;
                        cast_health = None;
                        route_to_sink(
                            &runner,
                            &mut loopback_module_id,
                            &mut active,
                            &available,
                            OutputKind::Soundcard,
                            &id,
                        )
                        .await;
                        state.set_output(OutputState {
                            active: active.clone(),
                            available: available.clone(),
                            cast_health: cast_health.clone(),
                        })
                        .await;
                    }

                    Some(OutputCommand::Select { kind: OutputKind::Airplay, id }) => {
                        // AirPlay routes identically to Soundcard: the RAOP PipeWire
                        // sinks created by module-raop-discover are regular sinks;
                        // routing is the same module-loopback mechanism.
                        teardown(&runner, &mut loopback_module_id, &mut cast_session).await;
                        cast_health = None;
                        route_to_sink(
                            &runner,
                            &mut loopback_module_id,
                            &mut active,
                            &available,
                            OutputKind::Airplay,
                            &id,
                        )
                        .await;
                        state.set_output(OutputState {
                            active: active.clone(),
                            available: available.clone(),
                            cast_health: cast_health.clone(),
                        })
                        .await;
                    }

                    Some(OutputCommand::Select { kind: OutputKind::Chromecast, id }) => {
                        // OUT-4b: CASTV2 session routing.
                        // Teardown any current loopback/cast (single-active across all kinds).
                        teardown(&runner, &mut loopback_module_id, &mut cast_session).await;
                        cast_health = None;

                        // Find the device to get its IPv4 address and port.
                        let device = find_device(&available, &OutputKind::Chromecast, &id);

                        match device {
                            None => {
                                // Device not present in discovery results at all.
                                eprintln!(
                                    "soundsync/output: Chromecast device {id:?} not found \
                                     in discovery — cannot start cast session"
                                );
                                active = None;
                            }
                            Some(ref dev) if dev.addr.is_none() => {
                                // Device found but has no IPv4 address yet.
                                eprintln!(
                                    "soundsync/output: Chromecast {id:?} has no IPv4 address — \
                                     cannot start cast session"
                                );
                                active = None;
                            }
                            Some(dev) => {
                                let addr = dev.addr.clone().unwrap();
                                let port = dev.port.unwrap_or(8009);

                                // Guard: refuse to start if the LAN IP is unknown —
                                // the resulting URL would be "http://:8080/..." which
                                // the Chromecast cannot pull.
                                if lan_ip.is_empty() {
                                    eprintln!(
                                        "soundsync/output: LAN IP unknown — Chromecast stream \
                                         URL will be invalid; cannot cast"
                                    );
                                    active = None;
                                } else {
                                    let url = stream_url(&lan_ip, bind_port);
                                    let session = start_cast(addr, port, url);
                                    cast_session = Some(session);
                                    // NF-8: alive but unconfirmed until/unless the
                                    // session task dies (then -> Lost on the tick).
                                    cast_health = Some(CastHealth::Connecting);
                                    active = Some(dev);
                                }
                            }
                        }

                        state.set_output(OutputState {
                            active: active.clone(),
                            available: available.clone(),
                            cast_health: cast_health.clone(),
                        }).await;
                    }
                }
            }

            // Periodic discovery refresh (also spawn_blocking to avoid blocking runtime).
            _ = refresh_interval.tick() => {
                let r = Arc::clone(&runner);
                if let Ok(fresh) = tokio::task::spawn_blocking(move || probe_outputs(&*r)).await {
                    available = fresh;
                }

                // NF-8: detect a dead cast session.  The session task `return`s on
                // EOF / read-error / write-failure / the NF-26 inbound deadline.
                // When that happens the watch::Sender inside the handle lingers and
                // `active` would otherwise still show the (silent) device.  Clear
                // active, drop the session, and surface health = Lost so the UI no
                // longer reports a dead cast as active.  Re-selecting starts fresh.
                if let Some(s) = &cast_session {
                    if s.is_finished() {
                        eprintln!(
                            "soundsync/output: cast session ended — clearing active output"
                        );
                        cast_session = None;
                        active = None;
                        cast_health = Some(CastHealth::Lost);
                    }
                }

                // If the active device is no longer in the discovered list, keep
                // it in state (it may come back) — the UI can show it as stale.
                state.set_output(OutputState {
                    active: active.clone(),
                    available: available.clone(),
                    cast_health: cast_health.clone(),
                }).await;
            }
        }
    }

    // ── Shutdown teardown ─────────────────────────────────────────────────────
    //
    // 1. Tear down the active loopback AND any cast session.
    teardown(&runner, &mut loopback_module_id, &mut cast_session).await;

    // 2. Unload module-raop-discover so RAOP sinks don't linger after exit.
    //    Best-effort: log on failure, never propagate.
    if let Some(mid) = raop_discover_module_id.take() {
        let r = Arc::clone(&runner);
        let id_str = mid.to_string();
        let result =
            tokio::task::spawn_blocking(move || r.run("pactl", &["unload-module", &id_str])).await;
        match result {
            Ok(Ok(out)) if out.success() => {}
            Ok(Ok(out)) => {
                eprintln!(
                    "soundsync/output: pactl unload-module {mid} (raop-discover) \
                     exited {} — ignoring",
                    out.status
                );
            }
            Ok(Err(e)) => {
                eprintln!(
                    "soundsync/output: failed to spawn pactl unload-module {mid} \
                     (raop-discover): {e}"
                );
            }
            Err(e) => {
                eprintln!(
                    "soundsync/output: spawn_blocking error for unload-module {mid} \
                     (raop-discover): {e}"
                );
            }
        }
    }
}

/// Combined teardown: stops the active loopback module AND any active cast session.
///
/// This is the single-active enforcement point: every `Select` arm and `None`
/// call this before starting the new route so only one output is ever active.
async fn teardown<R>(
    runner: &Arc<R>,
    loopback_module_id: &mut Option<u32>,
    cast_session: &mut Option<CastSession>,
) where
    R: CommandRunner + Send + Sync + 'static,
{
    // Stop cast session first (non-blocking — just sends a watch signal).
    // Taking the session out drops its JoinHandle too; we do NOT await the task
    // (teardown stays non-blocking, matching the existing style).
    if let Some(s) = cast_session.take() {
        s.stop();
    }
    // Then tear down the PipeWire loopback (may spawn_blocking).
    teardown_loopback(runner, loopback_module_id).await;
}

/// Route audio to `sink_id` via `module-loopback`.
///
/// Shared by `Soundcard` and `Airplay` arms — both route by loopback to a
/// PipeWire sink name.
///
/// Steps:
/// 1. Tear down any currently active loopback.
/// 2. Load `module-loopback source=soundsync-capture.monitor sink=<sink_id>`.
/// 3. Parse and store the module id.
/// 4. Set `active` — look up the device in `available`; synthesise a minimal
///    record if not found (device may have just disappeared).
///
/// On any failure: `loopback_module_id` and `active` are set to `None`.
/// State publication is the caller's responsibility.
async fn route_to_sink<R>(
    runner: &Arc<R>,
    loopback_module_id: &mut Option<u32>,
    active: &mut Option<OutputDevice>,
    available: &AvailableOutputs,
    kind: OutputKind,
    sink_id: &str,
) where
    R: CommandRunner + Send + Sync + 'static,
{
    // 1. Tear down whatever is currently running.
    teardown_loopback(runner, loopback_module_id).await;

    // 2. Load the new loopback.
    let args = build_loopback_args(sink_id);
    let r = Arc::clone(runner);
    let load_result = tokio::task::spawn_blocking(move || {
        let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
        r.run("pactl", &str_args)
    })
    .await;

    match load_result {
        Ok(Ok(out)) if out.success() => {
            // 3. Parse the module id.
            let mid = parse_loopback_module_id(&out.stdout);
            if mid.is_none() {
                // load-module succeeded but returned an unparseable id —
                // we cannot track/unload this loopback so it may be orphaned.
                // Treat as failure: revert to None.
                eprintln!(
                    "soundsync/output: load-module succeeded but returned \
                     unparseable module id {:?} — loopback may be orphaned \
                     (cannot track/unload); reverting active to None",
                    out.stdout.trim()
                );
                *loopback_module_id = None;
                *active = None;
                return;
            }
            *loopback_module_id = mid;

            // 4. Look up the device in the available list for its metadata.
            //    If not found (e.g. it just disappeared) synthesise a minimal record.
            *active =
                Some(
                    find_device(available, &kind, sink_id).unwrap_or_else(|| OutputDevice {
                        kind,
                        id: sink_id.to_string(),
                        name: sink_id.to_string(),
                        addr: None,
                        port: None,
                    }),
                );
        }
        Ok(Ok(out)) => {
            // pactl returned non-zero.
            eprintln!(
                "soundsync/output: pactl load-module failed (status {}) \
                 for sink {sink_id:?}: {}",
                out.status,
                out.stderr.trim()
            );
            *active = None;
        }
        Ok(Err(e)) => {
            eprintln!(
                "soundsync/output: failed to spawn pactl for loopback on \
                 sink {sink_id:?}: {e}"
            );
            *active = None;
        }
        Err(e) => {
            // spawn_blocking panic / task cancelled.
            eprintln!(
                "soundsync/output: spawn_blocking error for loopback on \
                 sink {sink_id:?}: {e}"
            );
            *active = None;
        }
    }
}

/// Tear down the current loopback module if any.
///
/// Runs `pactl unload-module <id>` via `spawn_blocking` (best-effort: errors
/// are logged but do not propagate).  Clears `loopback_module_id` regardless.
async fn teardown_loopback<R>(runner: &Arc<R>, loopback_module_id: &mut Option<u32>)
where
    R: CommandRunner + Send + Sync + 'static,
{
    if let Some(mid) = loopback_module_id.take() {
        let r = Arc::clone(runner);
        let id_str = mid.to_string();
        let result =
            tokio::task::spawn_blocking(move || r.run("pactl", &["unload-module", &id_str])).await;
        match result {
            Ok(Ok(out)) if out.success() => {}
            Ok(Ok(out)) => {
                eprintln!(
                    "soundsync/output: pactl unload-module {mid} exited {} — ignoring",
                    out.status
                );
            }
            Ok(Err(e)) => {
                eprintln!("soundsync/output: failed to spawn pactl unload-module {mid}: {e}");
            }
            Err(e) => {
                eprintln!("soundsync/output: spawn_blocking error for unload-module {mid}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{AvailableOutputs, OutputDevice, OutputKind};
    use crate::state::AppStateHandle;
    use crate::sys::command::{CommandOutput, CommandRunner};
    use std::io;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{mpsc, watch};

    fn make_soundcard(id: &str) -> OutputDevice {
        OutputDevice {
            kind: OutputKind::Soundcard,
            id: id.to_string(),
            name: id.to_string(),
            addr: None,
            port: None,
        }
    }

    // ── Sync-safe fake runner ─────────────────────────────────────────────────
    //
    // `FakeCommandRunner` from `sys::testing` uses `RefCell` internally and is
    // therefore `!Sync`.  `spawn_blocking` requires the captured value to be
    // `Send + Sync + 'static` (because `Arc<R>: Send` only when `R: Sync`).
    // We use a minimal inline `SyncFakeCommandRunner` backed by `Mutex` that
    // implements `CommandRunner + Send + Sync + 'static`.

    /// A scripted `CommandRunner` that is `Send + Sync` (uses `Mutex`).
    ///
    /// Rules are matched by (program, must_contain_args) in insertion order,
    /// exactly like `FakeCommandRunner::on`.  Unmatched calls return status 127.
    struct SyncFakeCommandRunner {
        rules: Mutex<Vec<(String, Vec<String>, CommandOutput)>>,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl SyncFakeCommandRunner {
        fn new() -> Self {
            Self {
                rules: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
            }
        }
        /// Add a rule: returns `out` when `program` is called with all
        /// `must_contain` args present.
        fn on(self, program: &str, must_contain: &[&str], out: CommandOutput) -> Self {
            self.rules.lock().unwrap().push((
                program.to_string(),
                must_contain.iter().map(|s| s.to_string()).collect(),
                out,
            ));
            self
        }
        /// All recorded (program, args) calls.
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CommandRunner for SyncFakeCommandRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|s| s.to_string()).collect(),
            ));
            let rules = self.rules.lock().unwrap();
            for (prog, must, out) in rules.iter() {
                if prog == program && must.iter().all(|m| args.contains(&m.as_str())) {
                    return Ok(out.clone());
                }
            }
            Ok(CommandOutput {
                status: 127,
                stdout: String::new(),
                stderr: format!("fake: no response programmed for `{program}`"),
            })
        }
    }

    /// Build a `SyncFakeCommandRunner` that:
    /// - Returns a one-soundcard pactl list
    /// - Returns module id "536870913" for `load-module`
    /// - Returns success for `unload-module`
    /// - Returns errors for avahi-browse (via status 127 fallback)
    fn runner_with_soundcard(sink: &str) -> Arc<SyncFakeCommandRunner> {
        let module_id = "536870913";
        Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: format!("0\t{sink}\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n"),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: format!("{module_id}\n"),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        )
    }

    // ── Unit tests for find_device ────────────────────────────────────────────

    #[test]
    fn find_device_returns_matching_soundcard() {
        let device = make_soundcard("alsa_output.test");
        let available = AvailableOutputs {
            soundcard: vec![device.clone()],
            airplay: vec![],
            chromecast: vec![],
        };
        assert_eq!(
            find_device(&available, &OutputKind::Soundcard, "alsa_output.test"),
            Some(device)
        );
    }

    #[test]
    fn find_device_returns_none_for_unknown_id() {
        let available = AvailableOutputs::default();
        assert_eq!(
            find_device(&available, &OutputKind::Soundcard, "ghost"),
            None
        );
    }

    // ── Integration-style controller tests ───────────────────────────────────
    //
    // These tests use `tokio::spawn` (multi-threaded runtime) because
    // `spawn_blocking` requires a `'static + Send` closure.  The
    // `SyncFakeCommandRunner` is `Send + Sync + 'static`, satisfying all bounds.

    /// Drive the controller: pre-buffer commands, drop sender, await completion.
    async fn run_controller(
        runner: Arc<SyncFakeCommandRunner>,
        cmds: Vec<OutputCommand>,
    ) -> crate::state::AppStateHandle {
        let state = AppStateHandle::new();
        let (tx, rx) = mpsc::channel::<OutputCommand>(16);
        let (_sd_tx, sd_rx) = watch::channel(false);

        for cmd in cmds {
            tx.send(cmd).await.unwrap();
        }
        drop(tx);

        tokio::time::timeout(
            Duration::from_secs(2),
            run_output_controller(
                rx,
                runner,
                state.clone(),
                sd_rx,
                "192.168.1.1".to_string(),
                8080,
            ),
        )
        .await
        .expect("controller timed out");

        state
    }

    #[tokio::test]
    async fn select_soundcard_becomes_active_with_loopback() {
        let runner = runner_with_soundcard("alsa_output.speaker");
        let state = run_controller(
            runner.clone(),
            vec![OutputCommand::Select {
                kind: OutputKind::Soundcard,
                id: "alsa_output.speaker".to_string(),
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(output.active.is_some());
        assert_eq!(output.active.unwrap().id, "alsa_output.speaker");

        // Verify load-module was called.
        let calls = runner.calls();
        let load_calls: Vec<_> = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "load-module"))
            .collect();
        assert!(
            !load_calls.is_empty(),
            "expected pactl load-module to be called"
        );
    }

    #[tokio::test]
    async fn select_unknown_sink_still_becomes_active_with_synthesised_device() {
        // If the sink isn't in the available list (device disappeared between
        // discovery and selection), the loopback still loads and we synthesise
        // a minimal active record.
        let runner = runner_with_soundcard("alsa_output.speaker");
        let state = run_controller(
            runner,
            vec![OutputCommand::Select {
                kind: OutputKind::Soundcard,
                id: "alsa_output.other".to_string(), // not in available
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(output.active.is_some());
        assert_eq!(output.active.unwrap().id, "alsa_output.other");
    }

    #[tokio::test]
    async fn none_command_clears_active() {
        let runner = runner_with_soundcard("alsa_output.speaker");
        let state = run_controller(
            runner.clone(),
            vec![
                OutputCommand::Select {
                    kind: OutputKind::Soundcard,
                    id: "alsa_output.speaker".to_string(),
                },
                OutputCommand::None,
            ],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(output.active.is_none());

        // Verify unload-module was called during teardown.
        let calls = runner.calls();
        let unload_calls: Vec<_> = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "unload-module"))
            .collect();
        assert!(
            !unload_calls.is_empty(),
            "expected pactl unload-module on None"
        );
    }

    #[tokio::test]
    async fn switching_soundcard_teardowns_previous_loopback() {
        // Select sink A then sink B: should unload-module for A before loading B.
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: "0\talsa_output.a\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n\
                                 1\talsa_output.b\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n"
                            .to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: "100\n".to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        );

        let state = run_controller(
            runner.clone(),
            vec![
                OutputCommand::Select {
                    kind: OutputKind::Soundcard,
                    id: "alsa_output.a".to_string(),
                },
                OutputCommand::Select {
                    kind: OutputKind::Soundcard,
                    id: "alsa_output.b".to_string(),
                },
            ],
        )
        .await;

        let output = state.snapshot().await.output;
        assert_eq!(
            output.active.map(|d| d.id).as_deref(),
            Some("alsa_output.b")
        );

        // load-module calls: raop-discover (startup) + loopback for A + loopback for B.
        let calls = runner.calls();
        let loads: Vec<_> = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "load-module"))
            .collect();
        let unloads: Vec<_> = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "unload-module"))
            .collect();
        assert_eq!(
            loads.len(),
            3,
            "expected 3 load-module calls: raop-discover + loopback(A) + loopback(B)"
        );
        // unload for A (before loading B) + unload for B (shutdown teardown) +
        // unload for raop-discover (shutdown teardown).
        assert_eq!(
            unloads.len(),
            3,
            "expected 3 unload-module calls: teardown(A) + shutdown(B) + raop-discover"
        );
    }

    #[tokio::test]
    async fn shutdown_signal_exits_controller() {
        let runner = Arc::new(SyncFakeCommandRunner::new());
        let state = AppStateHandle::new();
        let (_tx, rx) = mpsc::channel::<OutputCommand>(8);
        let (sd_tx, sd_rx) = watch::channel(false);

        // Signal shutdown before running — the controller should exit immediately.
        sd_tx.send(true).unwrap();

        tokio::time::timeout(
            Duration::from_millis(500),
            run_output_controller(rx, runner, state, sd_rx, String::new(), 8080),
        )
        .await
        .expect("controller did not exit on shutdown");
    }

    #[tokio::test]
    async fn load_module_failure_reverts_to_none() {
        // Simulate pactl returning non-zero for load-module.
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: "0\talsa_output.speaker\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n"
                            .to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 1,
                        stdout: String::new(),
                        stderr: "module not found".to_string(),
                    },
                ),
        );

        let state = run_controller(
            runner,
            vec![OutputCommand::Select {
                kind: OutputKind::Soundcard,
                id: "alsa_output.speaker".to_string(),
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(
            output.active.is_none(),
            "expected active=None after failed load-module"
        );
    }

    #[tokio::test]
    async fn unparseable_module_id_reverts_active_to_none() {
        // load-module exits 0 but stdout is "Module #100\n" — not a bare u32,
        // so parse_loopback_module_id returns None.  The controller must revert
        // to active=None rather than publishing a device we can never unload.
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: "0\talsa_output.speaker\tmodule-alsa-card\ts16le 2ch\tSUSPENDED\n"
                            .to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: "Module #100\n".to_string(), // unparseable as bare u32
                        stderr: String::new(),
                    },
                ),
        );

        let state = run_controller(
            runner,
            vec![OutputCommand::Select {
                kind: OutputKind::Soundcard,
                id: "alsa_output.speaker".to_string(),
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(
            output.active.is_none(),
            "expected active=None when module id is unparseable (revert worked)"
        );
    }

    /// Build a runner that handles raop sinks (for AirPlay tests).
    fn runner_with_airplay(sink: &str) -> Arc<SyncFakeCommandRunner> {
        let module_id = "536870913";
        Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: format!(
                            "0\t{sink}\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED\n"
                        ),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: format!("{module_id}\n"),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        )
    }

    #[tokio::test]
    async fn select_airplay_becomes_active_with_loopback() {
        // AirPlay routing is identical to Soundcard: teardown → loopback → active.
        let sink = "raop_sink.HP-L.local.192.168.1.107.7000";
        let runner = runner_with_airplay(sink);
        let state = run_controller(
            runner.clone(),
            vec![OutputCommand::Select {
                kind: OutputKind::Airplay,
                id: sink.to_string(),
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(output.active.is_some(), "expected active AirPlay device");
        let dev = output.active.unwrap();
        assert_eq!(dev.kind, OutputKind::Airplay);
        assert_eq!(dev.id, sink);

        // Verify load-module was called (for the loopback, not just raop-discover).
        let calls = runner.calls();
        let loads: Vec<_> = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "load-module"))
            .collect();
        // raop-discover (startup) + loopback (AirPlay select) = 2 loads.
        assert_eq!(loads.len(), 2, "expected 2 load-module calls for AirPlay");
    }

    #[tokio::test]
    async fn raop_discover_loaded_on_startup_and_unloaded_on_shutdown() {
        // The controller must load module-raop-discover at startup and unload it
        // on shutdown so RAOP sinks don't linger.
        let runner = runner_with_soundcard("alsa_output.speaker");
        run_controller(runner.clone(), vec![]).await;

        let calls = runner.calls();

        // Startup: load-module module-raop-discover.
        let loaded_raop = calls.iter().any(|(p, args)| {
            p == "pactl"
                && args.iter().any(|a| a == "load-module")
                && args.iter().any(|a| a == "module-raop-discover")
        });
        assert!(
            loaded_raop,
            "expected pactl load-module module-raop-discover on startup"
        );

        // Shutdown: unload-module for the raop-discover id.
        // The runner returns "536870913" for every load-module, so the id is 536870913.
        let unloaded_raop = calls.iter().any(|(p, args)| {
            p == "pactl"
                && args.iter().any(|a| a == "unload-module")
                && args.iter().any(|a| a == "536870913")
        });
        assert!(
            unloaded_raop,
            "expected pactl unload-module <raop-discover id> on shutdown"
        );
    }

    #[tokio::test]
    async fn airplay_routes_like_soundcard_same_loopback_mechanism() {
        // A soundcard select followed by an airplay select: both call load-module;
        // the soundcard loopback is torn down before the airplay loopback is loaded.
        let soundcard_sink = "alsa_output.usb-FiiO";
        let airplay_sink = "raop_sink.Lounge.local.192.168.1.232.7000";
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: format!(
                            "0\t{soundcard_sink}\tmodule-alsa-card\ts16le\tSUSPENDED\n\
                             1\t{airplay_sink}\tmodule-raop-sink\ts16le\tSUSPENDED\n"
                        ),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: "42\n".to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        );

        let state = run_controller(
            runner.clone(),
            vec![
                OutputCommand::Select {
                    kind: OutputKind::Soundcard,
                    id: soundcard_sink.to_string(),
                },
                OutputCommand::Select {
                    kind: OutputKind::Airplay,
                    id: airplay_sink.to_string(),
                },
            ],
        )
        .await;

        let output = state.snapshot().await.output;
        let dev = output
            .active
            .expect("expected active output after airplay select");
        assert_eq!(dev.kind, OutputKind::Airplay);
        assert_eq!(dev.id, airplay_sink);

        // load-module: raop-discover + soundcard loopback + airplay loopback = 3.
        let calls = runner.calls();
        let loads = calls
            .iter()
            .filter(|(p, args)| p == "pactl" && args.iter().any(|a| a == "load-module"))
            .count();
        assert_eq!(
            loads, 3,
            "raop-discover + soundcard loopback + airplay loopback"
        );
    }

    #[tokio::test]
    async fn airplay_loopback_failure_reverts_active_to_none() {
        // module-raop-discover SUCCEEDS (startup) but module-loopback FAILS (status 1)
        // when the user selects an AirPlay sink.  The route_to_sink revert must fire:
        // active must be None after the command is processed.
        //
        // Per-arg matching: "module-raop-discover" arg → success; any other
        // load-module call (the loopback) → failure status 1.
        let sink = "raop_sink.HP-L.local.192.168.1.107.7000";
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                // Discovery: one RAOP sink so the device is in available.
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: format!(
                            "0\t{sink}\tmodule-raop-sink\ts16le 2ch 44100Hz\tSUSPENDED\n"
                        ),
                        stderr: String::new(),
                    },
                )
                // Startup load of module-raop-discover → succeeds.
                .on(
                    "pactl",
                    &["load-module", "module-raop-discover"],
                    CommandOutput {
                        status: 0,
                        stdout: "999\n".to_string(),
                        stderr: String::new(),
                    },
                )
                // Any other load-module call (the loopback for AirPlay) → fails.
                .on(
                    "pactl",
                    &["load-module", "module-loopback"],
                    CommandOutput {
                        status: 1,
                        stdout: String::new(),
                        stderr: "loopback failed".to_string(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        );

        let state = run_controller(
            runner,
            vec![OutputCommand::Select {
                kind: OutputKind::Airplay,
                id: sink.to_string(),
            }],
        )
        .await;

        let output = state.snapshot().await.output;
        assert!(
            output.active.is_none(),
            "expected active=None when AirPlay loopback load-module fails"
        );
    }

    /// Build a runner that discovers exactly one Chromecast at `addr:port`.
    ///
    /// The avahi-browse line is in the parseable (`;`-separated) format
    /// `parse_chromecasts` expects.  `load-module`/`unload-module` succeed and
    /// `list short sinks` is empty so only the cast path is exercised.
    fn runner_with_chromecast(addr: &str, port: u16, id: &str) -> Arc<SyncFakeCommandRunner> {
        let avahi = format!(
            "=;eth0;IPv4;Cast;_googlecast._tcp;local;host.local;{addr};{port};\
             \"fn=Test Cast\" \"id={id}\"\n"
        );
        Arc::new(
            SyncFakeCommandRunner::new()
                .on(
                    "avahi-browse",
                    &["_googlecast._tcp"],
                    CommandOutput {
                        status: 0,
                        stdout: avahi,
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["load-module"],
                    CommandOutput {
                        status: 0,
                        stdout: "200\n".to_string(),
                        stderr: String::new(),
                    },
                )
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        )
    }

    /// NF-8: a Chromecast whose session task dies must clear `active` and set
    /// `cast_health = Lost` on the next refresh tick — the controller must no
    /// longer report a dead cast as active.
    ///
    /// The session is made to die deterministically by casting to a refused port
    /// (`127.0.0.1:1`): the TCP connect fails fast, so `run_cast_session`
    /// returns and `CastSession::is_finished()` becomes true.  We then advance
    /// virtual time past `REFRESH_INTERVAL` to fire the tick that observes it.
    #[tokio::test(start_paused = true)]
    async fn dead_cast_session_clears_active_on_tick() {
        let id = "deadcast01";
        let runner = runner_with_chromecast("127.0.0.1", 1, id);
        let state = AppStateHandle::new();
        // Subscribe BEFORE anything publishes so state transitions can be awaited
        // deterministically (recv wakes on publish; immune to CPU contention),
        // instead of a fragile fixed yield count that starves under build load.
        let mut events = state.subscribe();
        let (tx, rx) = mpsc::channel::<OutputCommand>(8);
        let (_sd_tx, sd_rx) = watch::channel(false);

        let controller = tokio::spawn(run_output_controller(
            rx,
            runner,
            state.clone(),
            sd_rx,
            "192.168.1.1".to_string(),
            8080,
        ));

        // Select the Chromecast — start_cast spawns the doomed session task.
        tx.send(OutputCommand::Select {
            kind: OutputKind::Chromecast,
            id: id.to_string(),
        })
        .await
        .unwrap();

        // Deterministically wait for active=Connecting (select succeeded + the
        // doomed session task was spawned). recv() wakes on each state publish, so
        // this never starves under CPU contention.
        loop {
            {
                let out = state.snapshot().await.output;
                if out.active.is_some() && out.cast_health == Some(CastHealth::Connecting) {
                    break;
                }
            }
            if let Err(tokio::sync::broadcast::error::RecvError::Closed) = events.recv().await {
                panic!("event bus closed before the Chromecast became active (Connecting)");
            }
        }

        // The doomed session task (TCP connect to :1) finishes near-instantly.
        // Park on the event bus: while idle, start_paused auto-advances virtual
        // time to the controller's next REFRESH_INTERVAL tick, which observes the
        // finished session and publishes active=None + Lost. Deterministic — no
        // fixed yield count, no manual time advance.
        loop {
            {
                let out = state.snapshot().await.output;
                if out.active.is_none() && out.cast_health == Some(CastHealth::Lost) {
                    break;
                }
            }
            if let Err(tokio::sync::broadcast::error::RecvError::Closed) = events.recv().await {
                panic!("event bus closed before the dead session was cleared (Lost)");
            }
        }

        // Drop the sender so the controller exits, then await it.
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), controller).await;
    }

    /// When `pactl load-module module-raop-discover` fails with non-zero status
    /// (e.g. "Failure: Entity exists" from a prior crashed daemon), the controller
    /// should fall back to `pactl list short modules`, find the pre-existing module
    /// id, adopt it (store in `raop_discover_module_id`), and unload it on shutdown.
    ///
    /// This test uses per-arg matching to distinguish:
    ///   - `load-module module-raop-discover`  → failure status 1
    ///   - `list short modules`                → output containing a raop-discover row
    ///   - `unload-module`                     → success
    ///   - `list short sinks`                  → empty (no discovery needed here)
    #[tokio::test]
    async fn raop_discover_already_loaded_is_adopted_and_unloaded_on_shutdown() {
        let adopted_id = "77";
        let runner = Arc::new(
            SyncFakeCommandRunner::new()
                // load-module module-raop-discover → fails (already loaded).
                .on(
                    "pactl",
                    &["load-module", "module-raop-discover"],
                    CommandOutput {
                        status: 1,
                        stdout: String::new(),
                        stderr: "Failure: Module already loaded".to_string(),
                    },
                )
                // list short modules → contains the pre-existing row.
                .on(
                    "pactl",
                    &["list", "short", "modules"],
                    CommandOutput {
                        status: 0,
                        stdout: format!(
                            "7\tmodule-always-sink\t\n\
                             {adopted_id}\tmodule-raop-discover\t\n"
                        ),
                        stderr: String::new(),
                    },
                )
                // list short sinks → empty (no soundcard/airplay).
                .on(
                    "pactl",
                    &["list", "short", "sinks"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                )
                // unload-module → success (for the adopted module on shutdown).
                .on(
                    "pactl",
                    &["unload-module"],
                    CommandOutput {
                        status: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                ),
        );

        run_controller(runner.clone(), vec![]).await;

        let calls = runner.calls();

        // load-module must have been attempted.
        let tried_load = calls.iter().any(|(p, args)| {
            p == "pactl"
                && args.iter().any(|a| a == "load-module")
                && args.iter().any(|a| a == "module-raop-discover")
        });
        assert!(
            tried_load,
            "expected pactl load-module module-raop-discover"
        );

        // list short modules must have been queried as fallback.
        let queried_list = calls.iter().any(|(p, args)| {
            p == "pactl" && args.iter().any(|a| a == "list") && args.iter().any(|a| a == "modules")
        });
        assert!(
            queried_list,
            "expected pactl list short modules fallback query"
        );

        // The adopted id must have been unloaded on shutdown.
        let unloaded_adopted = calls.iter().any(|(p, args)| {
            p == "pactl"
                && args.iter().any(|a| a == "unload-module")
                && args.iter().any(|a| a == adopted_id)
        });
        assert!(
            unloaded_adopted,
            "expected pactl unload-module {adopted_id} (adopted raop-discover) on shutdown"
        );
    }
}
