//! Real Bluetooth composition (bluetooth-design.md §1/§2/§5). Owns the
//! `bluer::Session`+adapter and runs the pairing window + BT manager + the
//! device->AppState bridge concurrently. Real D-Bus glue (hardware-validated);
//! only `device_infos_from` is unit-tested.
use crate::bluetooth::adapter::{set_class_of_device, set_hci_name};
use crate::bluetooth::agent::PairingGate;
use crate::bluetooth::bluer_agent::register_pairing_agent;
use crate::bluetooth::bluer_control::BluerControl;
use crate::bluetooth::bluer_devices::BluerDeviceControl;
use crate::bluetooth::devices::DeviceControl;
use crate::bluetooth::manager::{run_bt_manager, DeviceSnapshot};
use crate::bluetooth::name_config;
use crate::bluetooth::pairing::{run_pairing_window, PairingCommand};
use crate::state::{AppStateHandle, DeviceInfo};
use crate::sys::command::CommandRunner;
use crate::sys::supervisor::wait_for_shutdown;
use tokio::sync::{mpsc, watch};
use tokio::time::Duration;

/// Commands that can be sent to the Bluetooth service loop.
#[derive(Debug)]
pub enum BtCommand {
    /// Connect to a device by MAC address.
    Connect(String),
    /// Disconnect a device by MAC address.
    Disconnect(String),
    /// Remove (forget) a paired device by MAC address.
    Forget(String),
    /// Rename the Bluetooth adapter (alias + HCI name + persist).
    SetName(String),
}

/// Advertised Bluetooth device name (alias + HCI name). Fallback default only.
pub const DEVICE_NAME: &str = "SoundSync";
/// How often to re-probe the `bluez_input.*` audio-active set (AUD-010).
const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Compose and run the Bluetooth input path until shutdown: real adapter setup,
/// the window-gated pairing agent, the pairing window, the BT manager, and the
/// device->AppState bridge. Owns the `bluer::Session` (dropping it on return
/// unregisters the agent + tears down discovery). Errors creating the session/
/// adapter are returned; per-op setup failures are best-effort (logged).
///
/// `initial_name` is the fully-resolved device name (precedence:
/// persisted > `SOUNDSYNC_NAME` env > default), computed by the caller via
/// `config::resolve_name`. The `SetName` command still persists to file as
/// before; on next start the persisted value wins via `resolve_name`.
pub async fn run_bluetooth<R: CommandRunner>(
    runner: R,
    initial_name: String,
    state: AppStateHandle,
    pairing_rx: mpsc::Receiver<PairingCommand>,
    bt_rx: mpsc::Receiver<BtCommand>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let session = bluer::Session::new().await.map_err(|e| e.to_string())?;
    let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;

    let control = BluerControl::with_adapter(adapter.clone());
    let device_control = BluerDeviceControl::new(adapter.clone());

    // Use the caller-resolved name (persisted > SOUNDSYNC_NAME env > default).
    let name = initial_name;
    state.set_device_name(name.clone()).await;

    // Best-effort adapter setup (AUD-002/003). May require CAP_NET_ADMIN; log + go.
    let hci = control.hci_name();
    if let Err(e) = control.power_and_alias(&name).await {
        eprintln!("bluetooth: power/alias failed: {e}");
    }
    if let Err(e) = set_class_of_device(&runner, &hci) {
        eprintln!("bluetooth: set CoD failed (needs CAP_NET_ADMIN?): {e}");
    }
    if let Err(e) = set_hci_name(&runner, &hci, &name) {
        eprintln!("bluetooth: set HCI name failed: {e}");
    }

    // Pairing window state + gate + agent (auto-accept only while open).
    let (open_tx, open_rx) = watch::channel(false);
    let gate = PairingGate::new(open_rx);
    let _agent = register_pairing_agent(&session, gate)
        .await
        .map_err(|e| format!("register agent: {e}"))?;

    // Subscribe BEFORE discovery so no early event is dropped, then self-warm.
    let events = device_control.subscribe();
    if let Err(e) = device_control.start_discovery().await {
        eprintln!("bluetooth: start_discovery failed: {e}");
    }

    let (snap_tx, snap_rx) = watch::channel(Vec::new());

    // Run window + manager + bridge + command-loop concurrently until shutdown.
    tokio::join!(
        run_pairing_window(&control, pairing_rx, open_tx, shutdown.clone()),
        run_bt_manager(&runner, events, snap_tx, PROBE_INTERVAL, shutdown.clone()),
        run_device_bridge(snap_rx, state.clone(), shutdown.clone()),
        run_bt_command_loop(
            &control,
            &device_control,
            bt_rx,
            state,
            &runner,
            hci,
            shutdown.clone()
        ),
    );
    Ok(())
}

/// Dispatch `BtCommand`s until shutdown or the sender is dropped.
/// Borrows `control` + `device_control` for structured concurrency (same
/// lifetimes as `run_bluetooth`'s join!).
async fn run_bt_command_loop<R: CommandRunner>(
    control: &BluerControl,
    device_control: &BluerDeviceControl,
    mut bt_rx: mpsc::Receiver<BtCommand>,
    state: AppStateHandle,
    runner: &R,
    hci: String,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            cmd = bt_rx.recv() => {
                let Some(cmd) = cmd else { return }; // sender dropped
                match cmd {
                    BtCommand::Connect(addr) => {
                        if let Err(e) = device_control.connect(&addr).await {
                            eprintln!("bluetooth: connect {addr}: {e}");
                        }
                    }
                    BtCommand::Disconnect(addr) => {
                        if let Err(e) = device_control.disconnect(&addr).await {
                            eprintln!("bluetooth: disconnect {addr}: {e}");
                        }
                    }
                    BtCommand::Forget(addr) => {
                        if let Err(e) = device_control.remove(&addr).await {
                            eprintln!("bluetooth: forget {addr}: {e}");
                        }
                    }
                    BtCommand::SetName(n) => {
                        // Alias/HCI-name are best-effort; we still persist + publish so the
                        // name sticks across restarts even if the live update failed.
                        if let Err(e) = control.set_alias(&n).await {
                            eprintln!("bluetooth: set_alias failed: {e}");
                        }
                        // Best-effort HCI name (needs CAP_NET_ADMIN).
                        if let Err(e) = set_hci_name(runner, &hci, &n) {
                            eprintln!("bluetooth: set HCI name failed: {e}");
                        }
                        if let Err(e) = name_config::save_device_name(&n) {
                            eprintln!("bluetooth: save device name failed: {e}");
                        }
                        state.set_device_name(n).await;
                    }
                }
            }
        }
    }
}

/// Bridge manager snapshots to `AppState` (publishes `SystemEvent::BluetoothDevices`).
async fn run_device_bridge(
    mut snap_rx: watch::Receiver<Vec<DeviceSnapshot>>,
    state: AppStateHandle,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => return,
            changed = snap_rx.changed() => {
                if changed.is_err() {
                    return; // producer gone
                }
                let infos = device_infos_from(&snap_rx.borrow_and_update());
                state.set_bluetooth_devices(infos).await;
            }
        }
    }
}

/// Convert manager snapshots into the web wire DTO (`DeviceState::as_str`).
pub fn device_infos_from(snaps: &[DeviceSnapshot]) -> Vec<DeviceInfo> {
    snaps
        .iter()
        .map(|s| DeviceInfo {
            addr: s.addr.clone(),
            name: s.name.clone(),
            state: s.state.as_str().to_string(),
            icon: s.icon.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::device::DeviceState;
    use crate::bluetooth::manager::DeviceSnapshot;

    #[test]
    fn converts_snapshots_to_wire_dtos() {
        let snaps = vec![
            DeviceSnapshot {
                addr: "AA:BB".into(),
                name: Some("Phone".into()),
                state: DeviceState::AudioActive,
                icon: Some("phone".into()),
            },
            DeviceSnapshot {
                addr: "CC:DD".into(),
                name: None,
                state: DeviceState::Connected,
                icon: None,
            },
        ];
        let infos = device_infos_from(&snaps);
        assert_eq!(infos.len(), 2);
        assert_eq!(infos[0].addr, "AA:BB");
        assert_eq!(infos[0].name.as_deref(), Some("Phone"));
        assert_eq!(infos[0].state, "audio_active");
        assert_eq!(infos[0].icon.as_deref(), Some("phone"));
        assert_eq!(infos[1].name, None);
        assert_eq!(infos[1].state, "connected");
        assert_eq!(infos[1].icon, None);
    }

    #[test]
    fn empty_in_empty_out() {
        assert!(device_infos_from(&[]).is_empty());
    }
}
