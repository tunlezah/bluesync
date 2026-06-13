//! The device-management seam (discovery + connect/disconnect + a BlueZ
//! device-event stream). Real `bluer` impl in `bluer_devices`; fake in
//! `testing`. Static dispatch (RPITIT + Send), like `BluetoothControl` /
//! `ProcessSpawner` — no async-trait, no dyn. Events surface on a `broadcast`
//! channel so the manager (B-3c) can react event-driven (no polling).
use std::future::Future;
use tokio::sync::broadcast;

/// A BlueZ device lifecycle event observed on the adapter. (The `bluez_input.*`
/// PipeWire audio-node events come separately, from `audio_node::probe_*`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BluezEvent {
    /// A device appeared in discovery (address; name + BlueZ icon if known).
    DeviceAdded {
        addr: String,
        name: Option<String>,
        /// BlueZ `Icon` hint (e.g. "phone", "audio-card", "audio-headset"),
        /// used by the UI to pick a device-type glyph. `None` if unknown.
        icon: Option<String>,
    },
    /// A device was removed from the adapter.
    DeviceRemoved { addr: String },
    /// BlueZ reports the device connected.
    Connected { addr: String },
    /// BlueZ reports the device disconnected.
    Disconnected { addr: String },
}

/// Device discovery + connection control over BlueZ. The event stream is a
/// `broadcast` subscription (multiple consumers, lag-tolerant).
pub trait DeviceControl: Send + Sync {
    /// Begin device discovery (inquiry). Idempotent at the BlueZ level.
    fn start_discovery(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Stop device discovery.
    fn stop_discovery(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Connect to a device by MAC address (e.g. `"AA:BB:CC:DD:EE:FF"`).
    fn connect(&self, addr: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Disconnect a device by MAC address.
    fn disconnect(&self, addr: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Remove (forget) a paired device by MAC address.
    fn remove(&self, addr: &str) -> impl Future<Output = Result<(), String>> + Send;
    /// Subscribe to the BlueZ device-event stream.
    fn subscribe(&self) -> broadcast::Receiver<BluezEvent>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::testing::FakeDeviceControl;

    #[tokio::test]
    async fn fake_records_calls_in_order() {
        let dc = FakeDeviceControl::new();
        dc.start_discovery().await.unwrap();
        dc.connect("AA:BB:CC:DD:EE:FF").await.unwrap();
        dc.disconnect("AA:BB:CC:DD:EE:FF").await.unwrap();
        dc.remove("AA:BB:CC:DD:EE:FF").await.unwrap();
        dc.stop_discovery().await.unwrap();
        assert_eq!(
            dc.calls(),
            vec![
                "start_discovery".to_string(),
                "connect:AA:BB:CC:DD:EE:FF".to_string(),
                "disconnect:AA:BB:CC:DD:EE:FF".to_string(),
                "remove:AA:BB:CC:DD:EE:FF".to_string(),
                "stop_discovery".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn subscribers_receive_emitted_events() {
        let dc = FakeDeviceControl::new();
        let mut rx = dc.subscribe();
        dc.emit(BluezEvent::Connected {
            addr: "11:22:33:44:55:66".to_string(),
        });
        assert_eq!(
            rx.recv().await.unwrap(),
            BluezEvent::Connected {
                addr: "11:22:33:44:55:66".to_string()
            }
        );
    }

    #[tokio::test]
    async fn event_added_carries_optional_name() {
        let dc = FakeDeviceControl::new();
        let mut rx = dc.subscribe();
        dc.emit(BluezEvent::DeviceAdded {
            addr: "1".into(),
            name: Some("Phone".into()),
            icon: Some("phone".into()),
        });
        match rx.recv().await.unwrap() {
            BluezEvent::DeviceAdded { name, .. } => assert_eq!(name.as_deref(), Some("Phone")),
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
