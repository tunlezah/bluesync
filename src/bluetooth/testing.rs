//! Test fakes for the Bluetooth seam. `#[cfg(test)]`-only.
use super::control::BluetoothControl;
use super::devices::{BluezEvent, DeviceControl};
use std::sync::Mutex;
use tokio::sync::broadcast;

/// Records every `set_discoverable`/`set_pairable` call as `(op, on)`.
#[derive(Default)]
pub struct FakeBluetoothControl {
    calls: Mutex<Vec<(&'static str, bool)>>,
}

impl FakeBluetoothControl {
    pub fn new() -> Self {
        Self::default()
    }
    /// All recorded calls, in order.
    pub fn calls(&self) -> Vec<(&'static str, bool)> {
        self.calls.lock().unwrap().clone()
    }
}

impl BluetoothControl for FakeBluetoothControl {
    async fn set_discoverable(&self, on: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(("discoverable", on));
        Ok(())
    }
    async fn set_pairable(&self, on: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(("pairable", on));
        Ok(())
    }
}

/// Records discovery/connect/disconnect calls and lets tests inject `BluezEvent`s.
pub struct FakeDeviceControl {
    calls: Mutex<Vec<String>>,
    events: broadcast::Sender<BluezEvent>,
}

impl FakeDeviceControl {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(32);
        Self {
            calls: Mutex::new(Vec::new()),
            events,
        }
    }
    /// All recorded calls, in order (`"start_discovery"`, `"connect:AA:.."`, …).
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    /// Inject a synthetic BlueZ event to all current subscribers.
    pub fn emit(&self, ev: BluezEvent) {
        let _ = self.events.send(ev);
    }
}

impl Default for FakeDeviceControl {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceControl for FakeDeviceControl {
    async fn start_discovery(&self) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push("start_discovery".to_string());
        Ok(())
    }
    async fn stop_discovery(&self) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push("stop_discovery".to_string());
        Ok(())
    }
    async fn connect(&self, addr: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("connect:{addr}"));
        Ok(())
    }
    async fn disconnect(&self, addr: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("disconnect:{addr}"));
        Ok(())
    }
    async fn remove(&self, addr: &str) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("remove:{addr}"));
        Ok(())
    }
    fn subscribe(&self) -> broadcast::Receiver<BluezEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_records_calls() {
        let c = FakeBluetoothControl::new();
        c.set_discoverable(true).await.unwrap();
        c.set_pairable(false).await.unwrap();
        assert_eq!(c.calls(), vec![("discoverable", true), ("pairable", false)]);
    }
}
