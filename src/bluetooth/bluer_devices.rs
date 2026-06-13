//! Real `bluer`-backed implementation of `DeviceControl`. Compile-checked glue
//! — requires bluetoothd + an adapter at runtime. No unit tests here; the
//! testable contract lives in `devices::DeviceControl` + `testing::FakeDeviceControl`.
use crate::bluetooth::devices::{BluezEvent, DeviceControl};
use bluer::AdapterEvent;
use futures::StreamExt as _;
use std::collections::HashSet;
use tokio::sync::{broadcast, Mutex};
use tokio::task::{JoinHandle, JoinSet};

/// Spawn a per-device `Connected`/`Disconnected` watcher into `watchers`.
/// Aborted atomically when `watchers` is dropped (i.e. when the discovery task
/// is cancelled by `stop_discovery`).
///
/// When `Connected(true)` is observed the watcher emits `DeviceAdded{name}`
/// **then** `Connected` — this surfaces a brand-new phone that was not yet
/// paired/connected when its `AdapterEvent::DeviceAdded` fired (Fix 1b).
fn spawn_device_watcher(
    watchers: &mut JoinSet<()>,
    adapter: &bluer::Adapter,
    addr: bluer::Address,
    addr_str: String,
    tx: broadcast::Sender<BluezEvent>,
) {
    if let Ok(dev) = adapter.device(addr) {
        let adapter_clone = adapter.clone();
        watchers.spawn(async move {
            if let Ok(mut dev_events) = dev.events().await {
                while let Some(dev_ev) = dev_events.next().await {
                    match dev_ev {
                        bluer::DeviceEvent::PropertyChanged(bluer::DeviceProperty::Connected(
                            true,
                        )) => {
                            // Freshen the name at connection time: it may now be
                            // available even if it was absent at DeviceAdded time
                            // (brand-new phone completing pairing — Fix 1b).
                            let (name, icon) = match adapter_clone.device(addr) {
                                Ok(d) => {
                                    (d.name().await.ok().flatten(), d.icon().await.ok().flatten())
                                }
                                Err(_) => (None, None),
                            };
                            let _ = tx.send(BluezEvent::DeviceAdded {
                                addr: addr_str.clone(),
                                name,
                                icon,
                            });
                            let _ = tx.send(BluezEvent::Connected {
                                addr: addr_str.clone(),
                            });
                        }
                        bluer::DeviceEvent::PropertyChanged(bluer::DeviceProperty::Connected(
                            false,
                        )) => {
                            let _ = tx.send(BluezEvent::Disconnected {
                                addr: addr_str.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        });
    }
}

/// `DeviceControl` backed by `bluer` (D-Bus / BlueZ). Discovery runs in a
/// spawned task; dropping the stream stops BlueZ inquiry.
pub struct BluerDeviceControl {
    adapter: bluer::Adapter,
    events: broadcast::Sender<BluezEvent>,
    discovery: Mutex<Option<JoinHandle<()>>>,
}

impl BluerDeviceControl {
    pub fn new(adapter: bluer::Adapter) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            adapter,
            events,
            discovery: Mutex::new(None),
        }
    }
}

impl DeviceControl for BluerDeviceControl {
    /// Start PASSIVE BlueZ device watching (no active inquiry scan — an A2DP sink
    /// is passive; the phone initiates). Seeds paired/connected devices at startup,
    /// then follows `adapter.events()` for ongoing changes. Idempotent: a second
    /// call while already watching is a no-op.
    async fn start_discovery(&self) -> Result<(), String> {
        // Hold the tokio Mutex across the whole check→spawn→store sequence so
        // two concurrent callers cannot both pass the `is_some()` check and
        // each start a watcher task (TOCTOU fix).
        let mut guard = self.discovery.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let adapter = self.adapter.clone();
        let tx = self.events.clone();

        let handle = tokio::spawn(async move {
            // All per-device watcher tasks are owned here; when this task is
            // aborted (by stop_discovery) the JoinSet is dropped, which
            // cancels every watcher atomically — no leaks on restart.
            let mut watchers: JoinSet<()> = JoinSet::new();
            // Track addresses for which a watcher has already been spawned so
            // Phase 1 and Phase 2 never create duplicate watchers (Fix 3).
            let mut watched: HashSet<bluer::Address> = HashSet::new();

            // ── Phase 1: seed paired/connected devices ────────────────────
            // adapter.events() does NOT replay known devices, so seed them
            // explicitly. Only surface paired or currently-connected devices
            // (skip noisy cached-only BLE junk).
            let known_addrs = adapter.device_addresses().await.unwrap_or_default();
            for addr in known_addrs {
                let dev = match adapter.device(addr) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let paired = dev.is_paired().await.unwrap_or(false);
                let connected = dev.is_connected().await.unwrap_or(false);
                if !paired && !connected {
                    continue;
                }
                let addr_str = addr.to_string();
                let name = dev.name().await.ok().flatten();
                let icon = dev.icon().await.ok().flatten();
                let _ = tx.send(BluezEvent::DeviceAdded {
                    addr: addr_str.clone(),
                    name,
                    icon,
                });
                if connected {
                    let _ = tx.send(BluezEvent::Connected {
                        addr: addr_str.clone(),
                    });
                }
                // Spawn per-device watcher for connect/disconnect transitions.
                if watched.insert(addr) {
                    spawn_device_watcher(&mut watchers, &adapter, addr, addr_str, tx.clone());
                }
            }

            // ── Phase 2: watch ongoing adapter events (passive) ───────────
            let mut stream = match adapter.events().await {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("soundsync: bluetooth: adapter.events() failed: {e}");
                    return;
                }
            };
            while let Some(ev) = stream.next().await {
                match ev {
                    AdapterEvent::DeviceAdded(addr) => {
                        let dev = match adapter.device(addr) {
                            Ok(d) => d,
                            Err(_) => continue,
                        };
                        // Always spawn the watcher so that a freshly-pairing
                        // phone (not yet paired/connected at this moment) is
                        // observed when BlueZ later fires Connected=true.  The
                        // watcher itself will then emit DeviceAdded{name} +
                        // Connected (Fix 1a).  Dedup so Phase 1 seed + Phase 2
                        // event for the same addr never create two watchers (Fix 3).
                        let addr_str = addr.to_string();
                        if watched.insert(addr) {
                            spawn_device_watcher(
                                &mut watchers,
                                &adapter,
                                addr,
                                addr_str.clone(),
                                tx.clone(),
                            );
                        }
                        // For an already-active device, surface it immediately.
                        let paired = dev.is_paired().await.unwrap_or(false);
                        let connected = dev.is_connected().await.unwrap_or(false);
                        if paired || connected {
                            let name = dev.name().await.ok().flatten();
                            let icon = dev.icon().await.ok().flatten();
                            let _ = tx.send(BluezEvent::DeviceAdded {
                                addr: addr_str.clone(),
                                name,
                                icon,
                            });
                            if connected {
                                let _ = tx.send(BluezEvent::Connected {
                                    addr: addr_str.clone(),
                                });
                            }
                        }
                        // Not paired/connected yet → watcher will emit events
                        // once the Connected property fires.
                    }
                    AdapterEvent::DeviceRemoved(addr) => {
                        let _ = tx.send(BluezEvent::DeviceRemoved {
                            addr: addr.to_string(),
                        });
                    }
                    AdapterEvent::PropertyChanged(_) => {}
                }
            }
        });

        *guard = Some(handle);
        Ok(())
    }

    async fn stop_discovery(&self) -> Result<(), String> {
        // Lock, take the handle, release the guard, then abort — so we never
        // hold the Mutex across the abort() call.
        let handle = self.discovery.lock().await.take();
        if let Some(h) = handle {
            h.abort();
        }
        Ok(())
    }

    async fn connect(&self, addr: &str) -> Result<(), String> {
        let address: bluer::Address = addr
            .parse()
            .map_err(|e: bluer::InvalidAddress| e.to_string())?;
        let dev = self.adapter.device(address).map_err(|e| e.to_string())?;
        dev.connect().await.map_err(|e| e.to_string())?;
        let _ = dev.set_trusted(true).await; // best-effort: auto-reconnect on return
        Ok(())
    }

    async fn disconnect(&self, addr: &str) -> Result<(), String> {
        let address: bluer::Address = addr
            .parse()
            .map_err(|e: bluer::InvalidAddress| e.to_string())?;
        let dev = self.adapter.device(address).map_err(|e| e.to_string())?;
        dev.disconnect().await.map_err(|e| e.to_string())
    }

    async fn remove(&self, addr: &str) -> Result<(), String> {
        let address: bluer::Address = addr
            .parse()
            .map_err(|e: bluer::InvalidAddress| e.to_string())?;
        self.adapter
            .remove_device(address)
            .await
            .map_err(|e| e.to_string())
    }

    fn subscribe(&self) -> broadcast::Receiver<BluezEvent> {
        self.events.subscribe()
    }
}
