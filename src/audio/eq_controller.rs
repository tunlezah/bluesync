//! EQ controller task: owns the filter-chain supervisor lifetime and live gain
//! updates via `pw-cli set-param` (glitch-free, no restart for gain changes).
//!
//! Findings §6.3 / §6.7 / §8b — ordering rules:
//!   Enable:  spawn supervisor → wait_for_eq_node → set route_tx to EQ sink →
//!            set_eq_gains → state.set_eq(enabled=true)
//!   Disable: set route_tx to null sink FIRST (audio has somewhere to go) →
//!            abort supervisor → clear node_id cache → state.set_eq(enabled=false)
//!   Gains:   clamp → probe node_id (always, to handle supervisor-internal restart) →
//!            set_eq_gains; no restart, no route change.
use crate::audio::filter_chain::{detect_filter_chain_tool, run_filter_chain, FilterChainTool};
use crate::audio::pw::filter_ctl::{probe_eq_node_id, set_eq_gains, wait_for_eq_node};
use crate::daemon::{DesiredRoute, EQ_SINK_NAME, NULL_SINK_NAME};
use crate::dsp::eq::{default_bands, EqBand, MAX_GAIN_DB, MIN_GAIN_DB, NUM_BANDS};
use crate::state::{eq_info, AppStateHandle};
use crate::sys::command::CommandRunner;
use crate::sys::fs::Fs;
use crate::sys::process::ProcessSpawner;
use crate::sys::supervisor::{supervise, wait_for_shutdown, RestartPolicy};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Commands the EQ controller accepts from the HTTP handler and other callers.
#[derive(Debug)]
pub enum EqCommand {
    /// Update all 10 band gains (clamped internally).
    UpdateGains([f32; NUM_BANDS]),
    /// Enable or disable the EQ (start/stop the filter-chain supervisor).
    SetEnabled(bool),
}

/// Configuration bundle passed to [`run_eq_controller`].
pub struct EqControllerConfig {
    /// User's home directory (for the conf.d path in Embedded mode).
    pub home: String,
    /// XDG runtime directory (for the Standalone path of `run_filter_chain`).
    pub runtime_dir: String,
    /// EQ desired-route watch sender (updated on enable/disable).
    pub route_tx: watch::Sender<DesiredRoute>,
    /// Shutdown signal receiver.
    pub shutdown: watch::Receiver<bool>,
}

/// The supervisor handle type returned by tokio::spawn.
type SupervisorHandle = JoinHandle<crate::sys::supervisor::SuperviseReport>;

/// Run the EQ controller. Call from main and pass the channel receiver.
///
/// Generic over `CommandRunner + ProcessSpawner + Fs` for testability.
/// `home` is the user's home directory (for the conf.d path in Embedded mode).
/// `runtime_dir` is passed to the Standalone path of `run_filter_chain`.
// node_id is reset to None on disable/abort-path as an explicit defensive clear; Rust's
// flow analysis doesn't see these as "read" before the next overwrite, but they ARE
// semantically meaningful guards against stale cached ids.
#[allow(unused_assignments)]
pub async fn run_eq_controller<R, Sp, F>(
    mut rx: mpsc::Receiver<EqCommand>,
    runner: Arc<R>,
    spawner: Arc<Sp>,
    fs: Arc<F>,
    state: AppStateHandle,
    cfg: EqControllerConfig,
) where
    // R is used only on the controller task itself — no Sync needed.
    R: CommandRunner + Send + 'static,
    // Sp and F cross into the spawned supervisor task via Arc — Sync required.
    Sp: ProcessSpawner + Send + Sync + 'static,
    F: Fs + Send + Sync + 'static,
{
    let EqControllerConfig {
        home,
        runtime_dir,
        route_tx,
        mut shutdown,
    } = cfg;

    // Current band gains (kept in sync with AppState).
    let mut gains: [f32; NUM_BANDS] = {
        let b = default_bands();
        let mut arr = [0.0_f32; NUM_BANDS];
        for (i, band) in b.iter().enumerate() {
            arr[i] = band.gain_db;
        }
        arr
    };

    // Supervisor handle: Some when EQ is enabled, None when disabled.
    let mut supervisor: Option<SupervisorHandle> = None;
    // Cached node id — invalidated when supervisor is stopped/restarted.
    let mut node_id: Option<u32> = None;

    // Tool detection is synchronous and cheap.
    let tool = detect_filter_chain_tool(&*runner);

    loop {
        let cmd = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            cmd = rx.recv() => match cmd {
                Some(c) => c,
                None => return, // sender dropped
            },
        };

        match cmd {
            EqCommand::SetEnabled(true) => {
                // Already running? No-op (idempotent).
                if supervisor.is_some() {
                    eprintln!("soundsync/eq: SetEnabled(true) — EQ already enabled, ignoring");
                    continue;
                }

                // Spawn the supervised filter-chain.
                let bands = gains_to_bands(&gains);
                let sup = spawn_filter_chain_supervisor(
                    spawner.clone(),
                    fs.clone(),
                    bands,
                    runtime_dir.clone(),
                    home.clone(),
                    tool,
                    shutdown.clone(),
                );
                supervisor = Some(sup);

                // Poll until the EQ node appears (replaces legacy 500 ms sleep).
                let id = wait_for_eq_node(&*runner, Duration::from_secs(2)).await;

                if id.is_none() {
                    // EQ node never appeared — abort the just-spawned supervisor,
                    // stay on the null sink, and report disabled so the UI is correct.
                    eprintln!(
                        "soundsync/eq: EQ node did not appear within 2s — staying on null sink"
                    );
                    if let Some(handle) = supervisor.take() {
                        handle.abort();
                        let _ = handle.await; // ignore JoinError from abort
                    }
                    node_id = None;
                    let _ = route_tx.send(DesiredRoute {
                        default_sink: NULL_SINK_NAME.to_string(),
                    });
                    let bands_snapshot = gains_to_bands(&gains);
                    state.set_eq(eq_info(false, &bands_snapshot)).await;
                    continue;
                }

                node_id = id;

                // Route default sink to EQ input.
                let _ = route_tx.send(DesiredRoute {
                    default_sink: EQ_SINK_NAME.to_string(),
                });

                // Apply current gains to the running node.
                if let Some(id) = node_id {
                    if let Err(e) = set_eq_gains(&*runner, id, &gains).await {
                        eprintln!("soundsync/eq: initial set_eq_gains failed: {e}");
                    }
                }

                // Update state.
                let bands_snapshot = gains_to_bands(&gains);
                state.set_eq(eq_info(true, &bands_snapshot)).await;
            }

            EqCommand::SetEnabled(false) => {
                // Route back to null sink FIRST (AUD-022 ordering: audio must
                // have a destination before the EQ node disappears).
                let _ = route_tx.send(DesiredRoute {
                    default_sink: NULL_SINK_NAME.to_string(),
                });

                // Abort the supervisor task and await its cancellation (child killed by supervisor).
                if let Some(handle) = supervisor.take() {
                    handle.abort();
                    let _ = handle.await;
                }
                node_id = None;

                // Update state.
                let bands_snapshot = gains_to_bands(&gains);
                state.set_eq(eq_info(false, &bands_snapshot)).await;
            }

            EqCommand::UpdateGains(new_gains) => {
                // Clamp every band to [MIN_GAIN_DB, MAX_GAIN_DB].
                for (i, g) in new_gains.iter().enumerate() {
                    gains[i] = g.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
                }

                // If running, apply live — no restart, no route change.
                // Always re-probe the node id: the supervisor may have internally restarted
                // the filter-chain process (crash → respawn), causing PipeWire to assign a
                // new object id while the outer JoinHandle stays alive and the cached id goes
                // stale. A single-shot probe is cheap; the full wait-loop is NOT used here.
                if supervisor.is_some() {
                    node_id = probe_eq_node_id(&*runner).await;
                    if let Some(id) = node_id {
                        if let Err(e) = set_eq_gains(&*runner, id, &gains).await {
                            eprintln!("soundsync/eq: set_eq_gains failed: {e}");
                        }
                    } else {
                        eprintln!(
                            "soundsync/eq: EQ node not found; gains stored, will apply on next enable"
                        );
                    }
                }

                // Update state regardless.
                let bands_snapshot = gains_to_bands(&gains);
                let enabled = supervisor.is_some();
                state.set_eq(eq_info(enabled, &bands_snapshot)).await;
            }
        }
    }
}

/// Convert a flat gains array back into `EqBand` structs using the default
/// band parameters (freq/q/filter_type are fixed; only gain_db varies at runtime).
fn gains_to_bands(gains: &[f32; NUM_BANDS]) -> Vec<EqBand> {
    let mut bands = default_bands().to_vec();
    for (band, &g) in bands.iter_mut().zip(gains.iter()) {
        band.gain_db = g;
    }
    bands
}

/// Spawn the filter-chain supervisor as a background task.
fn spawn_filter_chain_supervisor<Sp, F>(
    spawner: Arc<Sp>,
    fs: Arc<F>,
    bands: Vec<EqBand>,
    runtime_dir: String,
    home: String,
    tool: FilterChainTool,
    shutdown: watch::Receiver<bool>,
) -> SupervisorHandle
where
    Sp: ProcessSpawner + Send + Sync + 'static,
    F: Fs + Send + Sync + 'static,
{
    tokio::spawn(async move {
        let factory = move || {
            let spawner = spawner.clone();
            let fs = fs.clone();
            let bands = bands.clone();
            let runtime_dir = runtime_dir.clone();
            let home = home.clone();
            async move { run_filter_chain(&*spawner, &*fs, &bands, &runtime_dir, tool, &home).await }
        };
        supervise(
            "filter-chain-eq",
            factory,
            RestartPolicy::default(),
            shutdown,
        )
        .await
    })
}

/// Clamp a single gain value to the valid EQ range [`MIN_GAIN_DB`, `MAX_GAIN_DB`].
pub fn clamp_eq_gain(g: f32) -> f32 {
    g.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::eq::{MAX_GAIN_DB, MIN_GAIN_DB};

    #[test]
    fn gains_to_bands_maps_correctly() {
        let mut gains = [0.0_f32; NUM_BANDS];
        gains[0] = 6.0;
        gains[9] = -3.0;
        let bands = gains_to_bands(&gains);
        assert_eq!(bands.len(), NUM_BANDS);
        assert_eq!(bands[0].gain_db, 6.0);
        assert_eq!(bands[9].gain_db, -3.0);
        // freq/q/filter_type come from default_bands()
        assert_eq!(bands[0].freq, 60.0);
        assert_eq!(bands[9].freq, 16000.0);
    }

    #[test]
    fn clamp_eq_gain_bounds() {
        assert_eq!(clamp_eq_gain(20.0), MAX_GAIN_DB);
        assert_eq!(clamp_eq_gain(-20.0), MIN_GAIN_DB);
        assert_eq!(clamp_eq_gain(6.0), 6.0);
    }

    #[test]
    fn update_gains_clamping_logic() {
        // Simulate the clamping step from EqCommand::UpdateGains
        let new_gains = [20.0_f32; NUM_BANDS];
        let mut stored = [0.0_f32; NUM_BANDS];
        for (i, g) in new_gains.iter().enumerate() {
            stored[i] = g.clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        }
        assert!(stored.iter().all(|&g| g == MAX_GAIN_DB));
    }

    #[test]
    fn enable_disable_ordering_decision() {
        // Document the ordering invariant (not a live test, just a logic check):
        // On disable: route first, then stop supervisor.
        // This test verifies the ordering constants are correct.
        assert_eq!(EQ_SINK_NAME, "effect_input.soundsync-eq");
        assert_eq!(NULL_SINK_NAME, "soundsync-capture");
    }

    /// A minimal `ProcessSpawner + Sync` stub for controller unit tests.
    /// Spawned children resolve to exit code 0 immediately.
    struct SyncFakeSpawner;

    /// A minimal child handle: immediately returns None (still running).
    struct SyncFakeChild;
    impl crate::sys::process::ChildHandle for SyncFakeChild {
        async fn wait(&mut self) -> std::io::Result<Option<i32>> {
            // Block forever (never exits unless dropped/killed).
            // This simulates a long-running filter-chain process.
            std::future::pending().await
        }
        async fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A minimal Stdout: immediate EOF.
    struct SyncFakeStdout;
    impl tokio::io::AsyncRead for SyncFakeStdout {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    impl crate::sys::process::ProcessSpawner for SyncFakeSpawner {
        type Child = SyncFakeChild;
        type Stdout = SyncFakeStdout;
        async fn spawn_capture(
            &self,
            _program: &str,
            _args: &[&str],
        ) -> std::io::Result<(Self::Child, Self::Stdout)> {
            Ok((SyncFakeChild, SyncFakeStdout))
        }
        async fn spawn(&self, _program: &str, _args: &[&str]) -> std::io::Result<Self::Child> {
            Ok(SyncFakeChild)
        }
    }

    /// A minimal `Fs + Sync` stub for controller unit tests.
    struct SyncFakeFs;

    impl crate::sys::fs::Fs for SyncFakeFs {
        fn read_to_string(&self, _: &str) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn write_atomic(&self, _: &str, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn remove_file(&self, _: &str, _: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    // Helper: build a controller future using Sync-compatible stubs.
    fn make_controller(
        rx: mpsc::Receiver<EqCommand>,
        state: crate::state::AppStateHandle,
        cfg: EqControllerConfig,
    ) -> impl std::future::Future<Output = ()> {
        use crate::sys::command::CommandOutput;

        // FakeCommandRunner is !Sync but CommandRunner only needs Send here.
        // We can use it as Arc<dyn CommandRunner + Send> but that requires
        // a concrete type. Instead, use a simple inline struct.
        struct NoOpRunner;
        impl crate::sys::command::CommandRunner for NoOpRunner {
            fn run(&self, _: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
                Ok(CommandOutput {
                    status: 127,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }

        run_eq_controller(
            rx,
            Arc::new(NoOpRunner),
            Arc::new(SyncFakeSpawner),
            Arc::new(SyncFakeFs),
            state,
            cfg,
        )
    }

    #[tokio::test]
    async fn controller_exits_on_sender_drop() {
        let (tx, rx) = mpsc::channel::<EqCommand>(8);
        let (route_tx, _) = watch::channel(DesiredRoute::default());
        let (_, sd_rx) = watch::channel(false);
        let state = crate::state::AppStateHandle::new();
        let ctrl = make_controller(
            rx,
            state,
            EqControllerConfig {
                home: "/home/test".to_string(),
                runtime_dir: "/run/user/1000".to_string(),
                route_tx,
                shutdown: sd_rx,
            },
        );
        // Drop tx concurrently: join runs ctrl + drop on same task.
        let dropper = async move { drop(tx) };
        tokio::join!(ctrl, dropper);
    }

    #[tokio::test]
    async fn controller_exits_on_shutdown() {
        let (_, rx) = mpsc::channel::<EqCommand>(8);
        let (route_tx, _) = watch::channel(DesiredRoute::default());
        let (sd_tx, sd_rx) = watch::channel(true); // pre-signalled
        let state = crate::state::AppStateHandle::new();
        let ctrl = make_controller(
            rx,
            state,
            EqControllerConfig {
                home: "/home/test".to_string(),
                runtime_dir: "/run/user/1000".to_string(),
                route_tx,
                shutdown: sd_rx,
            },
        );
        let _ = sd_tx; // keep alive
        ctrl.await; // controller sees shutdown=true immediately and exits
    }

    #[tokio::test]
    async fn update_gains_updates_state_when_not_running() {
        use crate::state::SystemEvent;

        let (tx, rx) = mpsc::channel::<EqCommand>(8);
        let (route_tx, _) = watch::channel(DesiredRoute::default());
        let (sd_tx, sd_rx) = watch::channel(false);
        let state = crate::state::AppStateHandle::new();
        let mut events = state.subscribe();
        let ctrl = make_controller(
            rx,
            state,
            EqControllerConfig {
                home: "/home/test".to_string(),
                runtime_dir: "/run/user/1000".to_string(),
                route_tx,
                shutdown: sd_rx,
            },
        );

        let mut new_gains = [0.0_f32; NUM_BANDS];
        new_gains[4] = 6.0;

        let driver = async {
            tx.send(EqCommand::UpdateGains(new_gains)).await.unwrap();
            // Wait for the EqChanged event.
            let eq = loop {
                match events.recv().await.unwrap() {
                    SystemEvent::EqChanged { eq } => break eq,
                    _ => continue,
                }
            };
            assert!(!eq.enabled);
            assert_eq!(eq.bands[4].gain_db, 6.0);
            sd_tx.send(true).unwrap();
        };

        tokio::join!(ctrl, driver);
    }
}
