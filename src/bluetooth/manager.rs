//! The Bluetooth manager state (bluetooth-design.md §3/§4). Tracks each known
//! device's `DeviceState` (passive sink: never auto-connects discovered devices)
//! and reconciles audio-active state from the `bluez_input.*` probe (AUD-010)
//! via the validated `apply_event` reducer.
//! Pure/synchronous — the async wiring is `run_bt_manager`.
use crate::bluetooth::audio_node::probe_bluez_input_addresses;
use crate::bluetooth::device::DeviceState;
use crate::bluetooth::devices::BluezEvent;
use crate::bluetooth::events::{apply_event, DeviceEvent};
use crate::sys::command::CommandRunner;
use crate::sys::supervisor::wait_for_shutdown;
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::{broadcast, watch};
use tokio::time::Duration;

/// A device's current view, for publishing to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub addr: String,
    pub name: Option<String>,
    pub state: DeviceState,
    /// BlueZ `Icon` hint (e.g. "phone", "audio-card"); last-known value is kept
    /// (sticky) so the UI glyph survives a disconnect.
    pub icon: Option<String>,
}

/// Tracks device states (keyed by MAC, sorted for deterministic snapshots).
#[derive(Default)]
pub struct BtManager {
    devices: BTreeMap<String, DeviceSnapshot>,
}

impl BtManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle a BlueZ event (passive sink: track state only, never auto-connect).
    /// BlueZ connect/disconnect are ground truth; discovery records `Discovered`.
    pub fn on_event(&mut self, ev: &BluezEvent) {
        match ev {
            BluezEvent::DeviceAdded { addr, name, icon } => {
                let snap = self
                    .devices
                    .entry(addr.clone())
                    .or_insert_with(|| DeviceSnapshot {
                        addr: addr.clone(),
                        name: name.clone(),
                        state: DeviceState::Disconnected,
                        icon: icon.clone(),
                    });
                // Only overwrite name/icon when this event carries one, so a
                // later event without it never clobbers a known value (sticky).
                if name.is_some() {
                    snap.name = name.clone();
                }
                if icon.is_some() {
                    snap.icon = icon.clone();
                }
                if snap.state == DeviceState::Disconnected {
                    snap.state = DeviceState::Discovered;
                }
            }
            BluezEvent::Connected { addr } => self.set_state(addr, DeviceState::Connected),
            BluezEvent::Disconnected { addr } => self.set_state(addr, DeviceState::Disconnected),
            BluezEvent::DeviceRemoved { addr } => {
                self.devices.remove(addr);
            }
        }
    }

    /// Reconcile audio-active state from the set of MACs with a `bluez_input.*`
    /// node. Uses `apply_event` so `AudioActive` is only reachable from
    /// `Connected` and a vanished node steps back to `Connected` (never lower).
    pub fn reconcile_audio(&mut self, active: &BTreeSet<String>) {
        for snap in self.devices.values_mut() {
            let ev = if active.contains(&snap.addr) {
                DeviceEvent::AudioNodeAppeared
            } else {
                DeviceEvent::AudioNodeDisappeared
            };
            snap.state = apply_event(snap.state, ev);
        }
    }

    /// Current device states, sorted by address.
    pub fn snapshot(&self) -> Vec<DeviceSnapshot> {
        self.devices.values().cloned().collect()
    }

    /// The state of one device, if known (test/inspection helper).
    pub fn state_of(&self, addr: &str) -> Option<DeviceState> {
        self.devices.get(addr).map(|s| s.state)
    }

    fn set_state(&mut self, addr: &str, state: DeviceState) {
        self.devices
            .entry(addr.to_string())
            .and_modify(|s| s.state = state)
            .or_insert_with(|| DeviceSnapshot {
                addr: addr.to_string(),
                name: None,
                state,
                icon: None,
            });
    }
}

/// Drive the Bluetooth manager: react to `DeviceControl` events (passive sink:
/// track state only, no auto-connect) and, every `probe_interval`, re-probe
/// the `bluez_input.*` audio-active set (AUD-010) and reconcile. Publishes a
/// `DeviceSnapshot` list on `states_out` whenever it changes. Borrows its deps
/// (run inline or wrap in `Arc` to spawn). Returns on shutdown or when the event
/// stream closes.
///
/// The caller MUST obtain `events` via `devices.subscribe()` before starting the
/// event producer (e.g. before `start_discovery`), so no early events are dropped.
pub async fn run_bt_manager<R: CommandRunner>(
    runner: &R,
    mut events: broadcast::Receiver<BluezEvent>,
    states_out: watch::Sender<Vec<DeviceSnapshot>>,
    probe_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut manager = BtManager::new();
    let mut ticker = tokio::time::interval(probe_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            ev = events.recv() => match ev {
                Ok(ev) => {
                    manager.on_event(&ev);
                    let _ = states_out.send(manager.snapshot());
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = ticker.tick() => {
                match probe_bluez_input_addresses(runner).await {
                    Ok(active) => {
                        manager.reconcile_audio(&active);
                        let _ = states_out.send(manager.snapshot());
                    }
                    Err(e) => eprintln!("bluetooth: bluez_input probe failed: {e}"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::device::DeviceState;
    use crate::bluetooth::devices::BluezEvent;
    use std::collections::BTreeSet;

    fn added(addr: &str) -> BluezEvent {
        BluezEvent::DeviceAdded {
            addr: addr.to_string(),
            name: Some("Phone".to_string()),
            icon: Some("phone".to_string()),
        }
    }

    #[test]
    fn discovered_device_is_tracked_not_connected() {
        let mut m = BtManager::new();
        m.on_event(&added("AA:BB:CC:DD:EE:FF"));
        assert_eq!(
            m.state_of("AA:BB:CC:DD:EE:FF"),
            Some(DeviceState::Discovered)
        );
    }

    #[test]
    fn connected_event_sets_connected_even_without_discovery() {
        // A previously-paired device reconnecting (no DeviceAdded first).
        let mut m = BtManager::new();
        m.on_event(&BluezEvent::Connected {
            addr: "11:22:33:44:55:66".to_string(),
        });
        assert_eq!(
            m.state_of("11:22:33:44:55:66"),
            Some(DeviceState::Connected)
        );
    }

    #[test]
    fn disconnect_sets_disconnected_and_remove_drops_device() {
        let mut m = BtManager::new();
        m.on_event(&BluezEvent::Connected { addr: "x".into() });
        m.on_event(&BluezEvent::Disconnected { addr: "x".into() });
        assert_eq!(m.state_of("x"), Some(DeviceState::Disconnected));
        m.on_event(&BluezEvent::DeviceRemoved { addr: "x".into() });
        assert_eq!(m.state_of("x"), None);
    }

    #[test]
    fn reconcile_audio_drives_connected_to_audio_active_and_back() {
        let mut m = BtManager::new();
        m.on_event(&BluezEvent::Connected {
            addr: "AA:BB:CC:DD:EE:FF".into(),
        });
        let mut active = BTreeSet::new();
        active.insert("AA:BB:CC:DD:EE:FF".to_string());
        m.reconcile_audio(&active);
        assert_eq!(
            m.state_of("AA:BB:CC:DD:EE:FF"),
            Some(DeviceState::AudioActive)
        );
        // node vanishes -> back to Connected (not lower).
        m.reconcile_audio(&BTreeSet::new());
        assert_eq!(
            m.state_of("AA:BB:CC:DD:EE:FF"),
            Some(DeviceState::Connected)
        );
    }

    #[test]
    fn reconcile_audio_does_not_activate_discovered_device() {
        let mut m = BtManager::new();
        m.on_event(&added("c")); // Discovered, not Connected
        let mut active = BTreeSet::new();
        active.insert("c".to_string());
        m.reconcile_audio(&active);
        // can't jump Discovered -> AudioActive.
        assert_eq!(m.state_of("c"), Some(DeviceState::Discovered));
    }

    #[test]
    fn snapshot_is_sorted_and_carries_name() {
        let mut m = BtManager::new();
        m.on_event(&added("BB:..."));
        m.on_event(&added("AA:..."));
        let snap = m.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].addr, "AA:...");
        assert_eq!(snap[0].name.as_deref(), Some("Phone"));
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use crate::bluetooth::device::DeviceState;
    use crate::bluetooth::devices::{BluezEvent, DeviceControl};
    use crate::bluetooth::testing::FakeDeviceControl;
    use crate::sys::command::CommandOutput;
    use crate::sys::testing::FakeCommandRunner;
    use tokio::sync::watch;
    use tokio::time::Duration;

    fn pactl_with(node_line: &str) -> FakeCommandRunner {
        FakeCommandRunner::new().on(
            "pactl",
            &["list", "short", "sources"],
            CommandOutput {
                status: 0,
                stdout: node_line.to_string(),
                stderr: String::new(),
            },
        )
    }

    #[tokio::test(start_paused = true)]
    async fn discovered_device_published_without_connecting() {
        let devices = FakeDeviceControl::new();
        let runner = pactl_with("");
        let (states_tx, mut states_rx) = watch::channel(Vec::new());
        let (sd_tx, sd_rx) = watch::channel(false);
        let events = devices.subscribe();

        tokio::join!(
            run_bt_manager(
                &runner,
                events,
                states_tx,
                Duration::from_millis(100),
                sd_rx
            ),
            async {
                devices.emit(BluezEvent::DeviceAdded {
                    addr: "AA:BB:CC:DD:EE:FF".into(),
                    name: Some("Phone".into()),
                    icon: Some("phone".into()),
                });
                loop {
                    states_rx.changed().await.unwrap();
                    if !states_rx.borrow().is_empty() {
                        break;
                    }
                }
                assert!(devices.calls().iter().all(|c| !c.starts_with("connect:")));
                sd_tx.send(true).unwrap();
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn probe_tick_marks_connected_device_audio_active() {
        let devices = FakeDeviceControl::new();
        let runner =
            pactl_with("9\tbluez_input.AA_BB_CC_DD_EE_FF.a2dp-source\tPipeWire\ts16le\tRUNNING");
        let (states_tx, mut states_rx) = watch::channel(Vec::new());
        let (sd_tx, sd_rx) = watch::channel(false);
        let events = devices.subscribe();

        tokio::join!(
            run_bt_manager(
                &runner,
                events,
                states_tx,
                Duration::from_millis(100),
                sd_rx
            ),
            async {
                devices.emit(BluezEvent::Connected {
                    addr: "AA:BB:CC:DD:EE:FF".into(),
                });
                // Wait until a snapshot reports AudioActive (a probe tick fired).
                loop {
                    states_rx.changed().await.unwrap();
                    let snap = states_rx.borrow().clone();
                    if snap.iter().any(|d| d.state == DeviceState::AudioActive) {
                        break;
                    }
                }
                sd_tx.send(true).unwrap();
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn returns_on_shutdown() {
        let devices = FakeDeviceControl::new();
        let runner = pactl_with("");
        let (states_tx, _states_rx) = watch::channel(Vec::new());
        let (sd_tx, sd_rx) = watch::channel(false);
        let events = devices.subscribe();
        sd_tx.send(true).unwrap();
        // Must return promptly (biased shutdown), not hang.
        run_bt_manager(
            &runner,
            events,
            states_tx,
            Duration::from_millis(100),
            sd_rx,
        )
        .await;
    }
}
