//! The robust, re-openable, auto-expiring pairing window (bluetooth-design.md §2).
use crate::bluetooth::control::BluetoothControl;
use crate::sys::supervisor::wait_for_shutdown;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, Instant};

/// Commands to the pairing window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingCommand {
    /// Open (or re-open/extend) the window for `duration`.
    Open { duration: Duration },
    /// Close the window now.
    Close,
}

/// Drive the pairing window: while open, the adapter is discoverable + pairable
/// and `open_state` is `true` (the agent gates auto-accept on it); the window
/// auto-closes after its duration. `Open` is idempotent and re-openable any time
/// (re-asserts discoverable/pairable — recovers from adapter resets — and extends
/// the deadline). The closed state always drives discoverable/pairable OFF, so the
/// adapter can never get wedged "open". Returns on shutdown (closing cleanly) or
/// when the command sender drops.
pub async fn run_pairing_window<C: BluetoothControl>(
    control: &C,
    mut cmds: mpsc::Receiver<PairingCommand>,
    open_state: watch::Sender<bool>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        // CLOSED: ensure the adapter is not discoverable/pairable.
        let _ = control.set_discoverable(false).await;
        let _ = control.set_pairable(false).await;
        let _ = open_state.send(false);

        // Wait for an Open (ignore Close while already closed).
        let duration = loop {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => return,
                cmd = cmds.recv() => match cmd {
                    Some(PairingCommand::Open { duration }) => break duration,
                    Some(PairingCommand::Close) => continue,
                    None => return, // command sender gone
                },
            }
        };

        // OPEN: discoverable + pairable, armed for `duration`.
        let _ = control.set_discoverable(true).await;
        let _ = control.set_pairable(true).await;
        let _ = open_state.send(true);
        let deadline = tokio::time::sleep(duration);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                biased;
                _ = wait_for_shutdown(&mut shutdown) => {
                    let _ = control.set_discoverable(false).await;
                    let _ = control.set_pairable(false).await;
                    let _ = open_state.send(false);
                    return;
                }
                _ = &mut deadline => break, // expired -> back to CLOSED
                cmd = cmds.recv() => match cmd {
                    Some(PairingCommand::Open { duration }) => {
                        // Re-assert (robustness) + extend the deadline.
                        let _ = control.set_discoverable(true).await;
                        let _ = control.set_pairable(true).await;
                        deadline.as_mut().reset(Instant::now() + duration);
                    }
                    Some(PairingCommand::Close) => break, // -> CLOSED
                    None => break,                        // sender gone -> CLOSED then return
                },
            }
        }
        // loop back to CLOSED (drives discoverable/pairable OFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::testing::FakeBluetoothControl;
    use tokio::sync::{mpsc, watch};
    use tokio::time::Duration;

    // Count how many times discoverable was driven to `on`.
    fn count(calls: &[(&'static str, bool)], op: &str, on: bool) -> usize {
        calls.iter().filter(|(o, v)| *o == op && *v == on).count()
    }

    #[tokio::test(start_paused = true)]
    async fn open_then_auto_close_after_duration() {
        let control = FakeBluetoothControl::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (open_tx, open_rx) = watch::channel(false);
        let (sd_tx, sd_rx) = watch::channel(false);

        let driver = async {
            cmd_tx
                .send(PairingCommand::Open {
                    duration: Duration::from_secs(300),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(310)).await; // past the window
            sd_tx.send(true).unwrap();
        };
        tokio::join!(run_pairing_window(&control, cmd_rx, open_tx, sd_rx), driver);

        let calls = control.calls();
        // Exact trace: CLOSED(false) -> Open(true) -> expiry->CLOSED(false) -> shutdown.
        assert_eq!(count(&calls, "discoverable", true), 1); // opened once
        assert_eq!(count(&calls, "discoverable", false), 2); // initial close + auto-close
                                                             // open_rx ended at `false` (window closed / shutdown).
        assert!(!*open_rx.borrow());
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_close_command_closes_the_window() {
        let control = FakeBluetoothControl::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (open_tx, open_rx) = watch::channel(false);
        let (sd_tx, sd_rx) = watch::channel(false);

        let driver = async {
            cmd_tx
                .send(PairingCommand::Open {
                    duration: Duration::from_secs(300),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await; // still well within the window
            cmd_tx.send(PairingCommand::Close).await.unwrap(); // close early
            tokio::time::sleep(Duration::from_secs(1)).await;
            sd_tx.send(true).unwrap();
        };
        tokio::join!(run_pairing_window(&control, cmd_rx, open_tx, sd_rx), driver);

        // Closed before the 300s deadline: opened once, then driven closed.
        let calls = control.calls();
        assert_eq!(count(&calls, "discoverable", true), 1);
        assert!(count(&calls, "discoverable", false) >= 2); // initial + explicit close
        assert!(!*open_rx.borrow());
    }

    #[tokio::test(start_paused = true)]
    async fn reopen_extends_the_window() {
        let control = FakeBluetoothControl::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (open_tx, _open_rx) = watch::channel(false);
        let (sd_tx, sd_rx) = watch::channel(false);

        let driver = async {
            cmd_tx
                .send(PairingCommand::Open {
                    duration: Duration::from_secs(300),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(200)).await;
            // re-open before expiry → extends to ~now+300 (i.e. ~500 total)
            cmd_tx
                .send(PairingCommand::Open {
                    duration: Duration::from_secs(300),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(200)).await; // total 400 < 500 → still open
                                                                // No auto-close should have happened yet (only the initial open's discoverable=true).
            assert_eq!(count(&control.calls(), "discoverable", false), 1); // just the initial CLOSED-state off
            sd_tx.send(true).unwrap();
        };
        tokio::join!(run_pairing_window(&control, cmd_rx, open_tx, sd_rx), driver);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_closes_cleanly() {
        let control = FakeBluetoothControl::new();
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (open_tx, open_rx) = watch::channel(false);
        let (sd_tx, sd_rx) = watch::channel(false);
        let driver = async {
            cmd_tx
                .send(PairingCommand::Open {
                    duration: Duration::from_secs(300),
                })
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
            sd_tx.send(true).unwrap();
        };
        tokio::join!(run_pairing_window(&control, cmd_rx, open_tx, sd_rx), driver);
        assert!(!*open_rx.borrow()); // ended closed
        assert!(count(&control.calls(), "discoverable", false) >= 1);
    }
}
