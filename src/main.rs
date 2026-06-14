use soundsync::adapter_id::resolve_adapter;
use soundsync::audio::capture::new_pcm_bus;
use soundsync::audio::eq_controller::{run_eq_controller, EqCommand, EqControllerConfig};
use soundsync::audio::spectrum::run_spectrum;
use soundsync::bluetooth::name_config;
use soundsync::bluetooth::service::BtCommand;
use soundsync::cast::client::server_lan_ip;
use soundsync::cli::{parse_args, Command};
use soundsync::config::{resolve_bind, resolve_name, resolve_web_dir};
use soundsync::daemon::{run_daemon, DaemonCommand, DesiredRoute};
use soundsync::doctor::run_checks;
use soundsync::install::{apply_adapter_config, apply_wireplumber_config};
use soundsync::output::controller::run_output_controller;
use soundsync::output::OutputCommand;
use soundsync::state::{AppStateHandle, VERSION};
use soundsync::sys::command::{CommandRunner, SystemCommandRunner};
use soundsync::sys::fs::SystemFs;
use soundsync::sys::process::TokioProcessSpawner;
use soundsync::web::server::{serve, ServerConfig, WebContext};
use soundsync::web::webrtc::command::webrtc_command_loop;
use soundsync::web::webrtc::webrtc_rs::{install_crypto_provider, WebrtcController};
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, watch};

#[tokio::main]
async fn main() {
    // ── Dispatch subcommand ────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = parse_args(&args);

    match cmd {
        Command::Version => {
            println!("soundsync {VERSION}");
            return;
        }
        Command::Help => {
            print_help();
            return;
        }
        Command::Doctor => {
            let web_dir = resolve_web_dir(std::env::var("SOUNDSYNC_WEB_DIR").ok().as_deref());
            let web_dir_str = web_dir.to_string_lossy();
            let runner = SystemCommandRunner;
            let fs = SystemFs;
            let report = run_checks(&runner, &fs, &web_dir_str);
            print!("{report}");
            std::process::exit(if report.ok() { 0 } else { 1 });
        }
        Command::ApplyWpConfig => {
            let runner = SystemCommandRunner;
            let fs = SystemFs;
            match apply_wireplumber_config(&fs, &runner) {
                Ok(summary) => {
                    println!("{summary}");
                }
                Err(e) => {
                    eprintln!("soundsync apply-wireplumber-config: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Command::ApplyAdapterConfig => {
            let runner = SystemCommandRunner;
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            let persisted = std::fs::read_to_string(name_config::device_name_path(&home))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let name = resolve_name(std::env::var("SOUNDSYNC_NAME").ok().as_deref(), persisted);
            let hci = resolve_adapter();
            match apply_adapter_config(&runner, &hci, &name) {
                Ok(()) => {
                    println!("adapter configured: {hci} name={name:?}");
                }
                Err(e) => {
                    eprintln!("soundsync apply-adapter-config: {e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        Command::Run => {
            // Fall through to the daemon startup below.
        }
    }

    // ── Run daemon ─────────────────────────────────────────────────────────
    install_crypto_provider();

    // Resolve env-driven bind address (default: 0.0.0.0:8080).
    let bind = match resolve_bind(std::env::var("SOUNDSYNC_BIND").ok().as_deref()) {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("soundsync: {e}");
            std::process::exit(1);
        }
    };
    // Resolve env-driven web-UI directory (default: /usr/share/soundsync/webui).
    let web_dir = resolve_web_dir(std::env::var("SOUNDSYNC_WEB_DIR").ok().as_deref());

    let runner = SystemCommandRunner;
    let spawner = Arc::new(TokioProcessSpawner);

    let state = AppStateHandle::new();
    let pcm_bus = new_pcm_bus();

    // Shutdown channel — created here so both the Bluetooth spawn and the web
    // server can receive the signal before they start.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Spectrum task: subscribe before capture produces so no frames are missed.
    let spectrum_h = tokio::spawn(run_spectrum(
        pcm_bus.subscribe(),
        state.clone(),
        shutdown_rx.clone(),
    ));

    // WebRTC controller owns the bus + state; driven via a command channel.
    // The command loop observes shutdown so it can drain all sessions (stop_all).
    // The channel is created BEFORE the controller so the controller can hold a
    // `webrtc_tx` clone for the NF-7 `on_peer_connection_state_change(Failed)`
    // self-teardown; the same `webrtc_tx` is moved into `WebContext` below.
    let (webrtc_tx, webrtc_rx) = mpsc::channel(32);
    let controller = WebrtcController::new(pcm_bus.clone(), state.clone(), webrtc_tx.clone());
    let webrtc_h = tokio::spawn(webrtc_command_loop(
        controller,
        webrtc_rx,
        shutdown_rx.clone(),
    ));

    // Pairing command channel; consumed by run_bluetooth's pairing window.
    let (pairing_tx, pairing_rx) =
        mpsc::channel::<soundsync::bluetooth::pairing::PairingCommand>(16);

    // Bluetooth device-command channel (connect/disconnect/forget/rename).
    let (bt_tx, bt_rx) = mpsc::channel::<BtCommand>(16);

    // Daemon command channel (ReassertRouting / Identify).
    let (daemon_tx, daemon_rx) = mpsc::channel::<DaemonCommand>(16);

    // Media transport command channel; consumed by run_media_manager below.
    let (media_tx, media_rx) = mpsc::channel::<soundsync::bluetooth::avrcp::TransportCommand>(16);

    // EQ desired-route watch (EQ controller updates this; reconcile loop reads it).
    let (route_tx, route_rx) = watch::channel(DesiredRoute::default());

    // EQ controller command channel.
    let (eq_tx, eq_rx) = mpsc::channel::<EqCommand>(32);

    // Resolve the device name (precedence: persisted > SOUNDSYNC_NAME env > default).
    let bt_name = {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let persisted = std::fs::read_to_string(name_config::device_name_path(&home))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        resolve_name(std::env::var("SOUNDSYNC_NAME").ok().as_deref(), persisted)
    };

    // Bluetooth input path (real adapter): pairing window drains pairing_rx,
    // manager tracks devices, bridge publishes to AppState. Spawned before the
    // web server so /api/pair + /api/bluetooth/:addr/:action have live consumers.
    let bt_h = {
        let bt_state = state.clone();
        let bt_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(e) = soundsync::bluetooth::service::run_bluetooth(
                SystemCommandRunner,
                bt_name,
                bt_state,
                pairing_rx,
                bt_rx,
                bt_shutdown,
            )
            .await
            {
                eprintln!("soundsync: bluetooth: {e}");
            }
        })
    };

    // AVRCP media manager (own D-Bus connection): drains media_rx, publishes
    // now-playing to AppState. Spawned before the server so /api/media has a
    // live consumer.
    let media_h = {
        let media_state = state.clone();
        let media_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            soundsync::bluetooth::media::run_media_manager(media_rx, media_state, media_shutdown)
                .await;
        })
    };

    // EQ controller task.
    let eq_h = {
        let eq_state = state.clone();
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        let runtime_dir =
            std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/0".to_string());
        tokio::spawn(run_eq_controller(
            eq_rx,
            Arc::new(SystemCommandRunner),
            spawner.clone(),
            Arc::new(SystemFs),
            eq_state,
            EqControllerConfig {
                home,
                runtime_dir,
                route_tx,
                shutdown: shutdown_rx.clone(),
            },
        ))
    };

    // Output controller command channel + task (OUT-4b: Chromecast routing).
    //
    // Compute the server's LAN IPv4 at startup via `hostname -I`.  If this
    // fails (no IP yet) the controller still starts; Chromecast routing will
    // be unavailable but all other paths are unaffected.
    let lan_ip: String = {
        let runner = SystemCommandRunner;
        match runner.run("hostname", &["-I"]) {
            Ok(out) if out.success() => server_lan_ip(&out.stdout).unwrap_or_else(|| {
                eprintln!(
                    "soundsync: hostname -I returned no usable IPv4 ({:?}) — \
                         Chromecast stream URL will be invalid",
                    out.stdout.trim()
                );
                String::new()
            }),
            Ok(out) => {
                eprintln!(
                    "soundsync: hostname -I failed (status {}) — Chromecast unavailable",
                    out.status
                );
                String::new()
            }
            Err(e) => {
                eprintln!("soundsync: failed to run hostname -I: {e} — Chromecast unavailable");
                String::new()
            }
        }
    };
    let bind_port = bind.port();

    let (output_tx, output_rx) = mpsc::channel::<OutputCommand>(16);
    let output_h = {
        let out_state = state.clone();
        let out_shutdown = shutdown_rx.clone();
        tokio::spawn(run_output_controller(
            output_rx,
            Arc::new(SystemCommandRunner),
            out_state,
            out_shutdown,
            lan_ip,
            bind_port,
        ))
    };

    // HTTP/WS server (same-origin SPA + /api/status + /ws/status).
    let ctx = WebContext {
        state: state.clone(),
        webrtc_tx,
        pairing_tx,
        media_tx,
        eq_tx: Some(eq_tx),
        bt_tx,
        daemon_tx,
        output_tx: Some(output_tx),
        shutdown: shutdown_rx.clone(),
        stream_limit: Arc::new(tokio::sync::Semaphore::new(
            soundsync::web::stream::STREAM_MAX_CONCURRENT,
        )),
    };
    let server_cfg = ServerConfig { bind, web_dir };
    let bind_addr = server_cfg.bind;
    let web_h = tokio::spawn(async move {
        if let Err(e) = serve(server_cfg, ctx).await {
            // Distinguish the port-already-in-use case with an actionable message.
            if e.kind() == std::io::ErrorKind::AddrInUse {
                eprintln!(
                    "soundsync: failed to bind {bind_addr} \
                    (port in use? another instance/legacy running?): {e}"
                );
            } else {
                eprintln!("soundsync: web server error: {e}");
            }
        }
    });

    // Shutdown on SIGTERM/SIGINT (see note: graceful server shutdown is a future refinement).
    tokio::spawn(async move {
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("soundsync: cannot install SIGTERM handler: {e}");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        let _ = shutdown_tx.send(true);
    });

    // Hard shutdown backstop (W0.2). The normal path returns promptly once
    // `shutdown_rx` flips (async bring-up + graceful web/webrtc drain), but if
    // anything stalls — a wedged `pactl`, a stuck child — this timeout guarantees
    // the process exits before systemd escalates to SIGKILL (TimeoutStopSec=15s
    // in the unit MUST stay > this backstop). `exit(0)` skips Drop-based cleanup
    // (BlueZ agent unregister, discovery teardown, kill_on_drop reaping), so this
    // is a BACKSTOP only — the clean path above should always win in practice.
    let shutdown_timeout = resolve_shutdown_timeout();

    // `run_daemon` is the long-running main loop; it returns only after `shutdown_rx`
    // flips and the clean teardown completes. The backstop timer therefore must start
    // counting ONLY once shutdown is actually requested — wrapping the whole daemon in
    // a `timeout(...)` would force-exit it during NORMAL operation (regression: an 8s
    // restart loop). So we race the daemon against a future that waits for the shutdown
    // request and only THEN grants `shutdown_timeout` before forcing exit.
    let mut backstop_rx = shutdown_rx.clone();
    // Dedicated watch view for the critical-task supervisor (NF-2), taken BEFORE
    // `shutdown_rx` is moved into `run_daemon` below.
    let supervisor_rx = shutdown_rx.clone();
    let daemon_fut = run_daemon(&runner, spawner, pcm_bus, route_rx, daemon_rx, shutdown_rx);
    tokio::pin!(daemon_fut);

    let backstop = async {
        // Park until shutdown is requested. If the sender is dropped WITHOUT a request
        // (e.g. the signal handler failed to install), never force-exit — park forever.
        if backstop_rx.wait_for(|requested| *requested).await.is_err() {
            std::future::pending::<()>().await;
        }
        // Shutdown requested: allow the clean path this long to finish, then give up.
        tokio::time::sleep(shutdown_timeout).await;
    };

    // Critical-task supervisor (NF-2). If any of the long-running background tasks
    // exits while the daemon is still meant to be running, make the process fatal
    // so systemd (`Restart=always`) restarts the WHOLE process — recovering both a
    // clean early return and a panic (a panicked JoinHandle resolves to `Err`).
    // The SIGTERM-handler spawn is intentionally EXCLUDED: it legitimately ends
    // after sending the shutdown signal. The serve task returns `Ok(())` on a
    // graceful shutdown — that exit is classified Expected via the shutdown flag.
    let supervisor = async {
        let handles: Vec<(&str, futures::future::BoxFuture<'static, ()>)> = vec![
            (
                "spectrum",
                Box::pin(async move {
                    let _ = spectrum_h.await;
                }),
            ),
            (
                "webrtc",
                Box::pin(async move {
                    let _ = webrtc_h.await;
                }),
            ),
            (
                "bluetooth",
                Box::pin(async move {
                    let _ = bt_h.await;
                }),
            ),
            (
                "media",
                Box::pin(async move {
                    let _ = media_h.await;
                }),
            ),
            (
                "eq",
                Box::pin(async move {
                    let _ = eq_h.await;
                }),
            ),
            (
                "output",
                Box::pin(async move {
                    let _ = output_h.await;
                }),
            ),
            (
                "web",
                Box::pin(async move {
                    let _ = web_h.await;
                }),
            ),
        ];
        let (labels, futs): (Vec<&str>, Vec<_>) = handles.into_iter().unzip();
        let (_done, index, _rest) = futures::future::select_all(futs).await;
        let name = labels[index];

        // TOCTOU GUARD (the 8s-restart-loop regression risk): a task may return
        // because a channel SENDER dropped slightly BEFORE the shutdown watch
        // propagates `true` to our borrow (webrtc_command_loop on all senders
        // dropped; run_spectrum on pcm_bus closed). Yield so any in-flight watch
        // update lands, then RE-READ the flag before classifying.
        tokio::task::yield_now().await;
        let shutdown_requested = *supervisor_rx.borrow();
        classify_task_exit(shutdown_requested, name)
    };

    tokio::select! {
        res = &mut daemon_fut => match res {
            Ok(()) => {}
            Err(e) => {
                eprintln!("soundsync: {e}");
                std::process::exit(1);
            }
        },
        _ = backstop => {
            eprintln!(
                "soundsync: shutdown did not complete within {}s — forcing exit \
                 (some Drop-based cleanup skipped)",
                shutdown_timeout.as_secs()
            );
            std::process::exit(0);
        }
        exit = supervisor => match exit {
            TaskExit::Expected => {
                // A critical task exited during/after a requested shutdown — normal.
                // Fall through to let the clean daemon path + backstop own teardown.
                // Park so this arm does not win the race over the daemon's own exit.
                std::future::pending::<()>().await;
            }
            TaskExit::Fatal(name) => {
                eprintln!(
                    "soundsync: critical task {name} exited unexpectedly \
                     (no shutdown requested) — exiting for systemd restart"
                );
                std::process::exit(1);
            }
        }
    }
}

/// Default hard-shutdown backstop. Must stay strictly below the unit's
/// `TimeoutStopSec` (15s) so the app exits before systemd SIGKILLs it (W0.2).
const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 8;

/// Resolve the shutdown backstop from `SOUNDSYNC_SHUTDOWN_TIMEOUT` (seconds),
/// falling back to [`DEFAULT_SHUTDOWN_TIMEOUT_SECS`]. An unparseable or zero
/// value falls back to the default (a zero timeout would defeat the clean path).
fn resolve_shutdown_timeout() -> std::time::Duration {
    let secs = std::env::var("SOUNDSYNC_SHUTDOWN_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// What to do when a critical background task exits (NF-2).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskExit {
    /// The exit happened during/after a requested shutdown — normal; let the
    /// clean path + backstop own teardown.
    Expected,
    /// The task died while the daemon was still meant to be running — make the
    /// process fatal so systemd (`Restart=always`) restarts everything. Carries
    /// the task name for the log.
    Fatal(String),
}

/// Classify a critical-task exit (NF-2). PURE so it is unit-testable without
/// `process::exit`. If shutdown was requested the exit is `Expected`; otherwise
/// it is `Fatal`. The TOCTOU guard (yield + re-read the shutdown flag) lives at
/// the call site so the `shutdown_requested` passed here reflects the latest
/// observable state — a task that returns on a sender-drop CONCURRENT with a
/// normal shutdown must be classified `Expected`, never `Fatal` (that path is
/// exactly the old 8s-restart-loop regression).
fn classify_task_exit(shutdown_requested: bool, name: &str) -> TaskExit {
    if shutdown_requested {
        TaskExit::Expected
    } else {
        TaskExit::Fatal(name.to_string())
    }
}

fn print_help() {
    println!(
        "soundsync {VERSION}

USAGE:
    soundsync [SUBCOMMAND]

SUBCOMMANDS:
    (default)                 Run the daemon (Bluetooth A2DP receiver + web UI)
    doctor                    Self-check: verify binaries, PipeWire, WP config, adapter, web dir
    apply-wireplumber-config  Write the WirePlumber A2DP config (run as root at install)
    apply-adapter-config      Set Bluetooth adapter Class-of-Device + HCI name (run as root at boot)
    version, --version, -V    Print version and exit
    help, --help, -h          Print this help

ENVIRONMENT:
    SOUNDSYNC_BIND      Bind address (default: 0.0.0.0:8080)
    SOUNDSYNC_WEB_DIR   Web UI directory (default: /usr/share/soundsync/webui)
    SOUNDSYNC_NAME      Initial device name (overridden by persisted ~/.config/soundsync/device-name)
    SOUNDSYNC_SHUTDOWN_TIMEOUT  Hard-exit backstop in seconds (default: 8; must stay < unit TimeoutStopSec)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn default_backstop_is_below_unit_timeout_stop_sec() {
        // The app's hard backstop MUST be strictly less than the systemd unit's
        // TimeoutStopSec (15s) so the process exits before systemd SIGKILLs it.
        assert!(DEFAULT_SHUTDOWN_TIMEOUT_SECS < 15);
    }

    // The backstop future used in `main`: park until shutdown is requested, THEN
    // grant `timeout` before it would force-exit. Modelled exactly here so the
    // tests guard the real semantics (not the old whole-daemon-wrapping timeout).
    async fn backstop(mut rx: watch::Receiver<bool>, timeout: Duration) {
        if rx.wait_for(|requested| *requested).await.is_err() {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(timeout).await;
    }

    #[tokio::test(start_paused = true)]
    async fn backstop_never_fires_during_normal_operation() {
        // REGRESSION GUARD: with no shutdown requested, the backstop must NOT fire
        // even after a long time. (Wrapping the whole daemon in a timeout made it
        // force-exit every 8s during normal operation — the crash loop.)
        let (tx, rx) = watch::channel(false);
        let fired = tokio::select! {
            _ = backstop(rx, Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)) => true,
            _ = tokio::time::sleep(Duration::from_secs(3600)) => false,
        };
        assert!(
            !fired,
            "backstop must not fire while no shutdown is requested"
        );
        drop(tx); // keep the sender alive across the race above
    }

    #[tokio::test(start_paused = true)]
    async fn backstop_fires_after_shutdown_request_when_daemon_hangs() {
        // Shutdown requested + a daemon that never returns => the backstop fires.
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let hung = std::future::pending::<()>();
        let fired = tokio::select! {
            _ = backstop(rx, Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)) => true,
            _ = hung => false,
        };
        assert!(fired, "a hung shutdown must trip the backstop");
    }

    #[test]
    fn classify_task_exit_fatal_when_not_shutting_down() {
        // No shutdown requested => an exit is unexpected => Fatal(name).
        assert_eq!(
            classify_task_exit(false, "spectrum"),
            TaskExit::Fatal("spectrum".to_string())
        );
    }

    #[test]
    fn classify_task_exit_expected_during_shutdown() {
        // Shutdown requested => the exit is part of teardown => Expected.
        assert_eq!(classify_task_exit(true, "web"), TaskExit::Expected);
    }

    // Mirrors the real supervisor structure (select_all + yield + re-read the
    // shutdown flag + classify) so the tests guard the live semantics, NOT just
    // the pure branch function.
    async fn supervise_once(
        handles: Vec<(&'static str, futures::future::BoxFuture<'static, ()>)>,
        rx: watch::Receiver<bool>,
    ) -> TaskExit {
        let (labels, futs): (Vec<&str>, Vec<_>) = handles.into_iter().unzip();
        let (_done, index, _rest) = futures::future::select_all(futs).await;
        let name = labels[index];
        tokio::task::yield_now().await;
        let shutdown_requested = *rx.borrow();
        classify_task_exit(shutdown_requested, name)
    }

    #[tokio::test]
    async fn supervisor_fatal_on_unexpected_exit() {
        // A task returns while no shutdown is requested => Fatal(name).
        let (_tx, rx) = watch::channel(false);
        let h: Vec<(&str, futures::future::BoxFuture<'static, ()>)> =
            vec![("output", Box::pin(async {}))];
        assert_eq!(
            supervise_once(h, rx).await,
            TaskExit::Fatal("output".to_string())
        );
    }

    #[tokio::test]
    async fn supervisor_expected_on_exit_during_shutdown() {
        // Shutdown already requested => a task exit is Expected.
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let h: Vec<(&str, futures::future::BoxFuture<'static, ()>)> =
            vec![("web", Box::pin(async {}))];
        assert_eq!(supervise_once(h, rx).await, TaskExit::Expected);
    }

    #[tokio::test]
    async fn supervisor_expected_on_sender_drop_concurrent_with_shutdown() {
        // THE TOCTOU REGRESSION PATH: a task returns because a channel sender
        // dropped, CONCURRENT with a normal shutdown whose watch update has not
        // yet propagated to our borrow. The yield + re-read must observe the
        // late `true` and classify Expected (NOT Fatal — that was the 8s loop).
        let (flag_tx, rx) = watch::channel(false);
        // A command-style channel whose closure ends the task (like
        // webrtc_command_loop breaking when all senders drop).
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<()>(1);
        let task = tokio::spawn(async move {
            // Returns as soon as the sender drops (channel closed).
            let _ = cmd_rx.recv().await;
        });
        let h: Vec<(&str, futures::future::BoxFuture<'static, ()>)> = vec![(
            "webrtc",
            Box::pin(async move {
                let _ = task.await;
            }),
        )];

        // Set shutdown and drop the command sender in the SAME instant, so the
        // task's exit races the watch propagation.
        flag_tx.send(true).unwrap();
        drop(cmd_tx);

        assert_eq!(supervise_once(h, rx).await, TaskExit::Expected);
    }

    #[tokio::test(start_paused = true)]
    async fn prompt_clean_shutdown_wins_the_backstop_race() {
        // Shutdown requested + a daemon that returns within budget => clean path wins.
        let (tx, rx) = watch::channel(false);
        tx.send(true).unwrap();
        let clean_daemon = tokio::time::sleep(Duration::from_secs(1));
        let clean = tokio::select! {
            _ = clean_daemon => true,
            _ = backstop(rx, Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)) => false,
        };
        assert!(
            clean,
            "a daemon that returns within the budget must win the race"
        );
    }
}
