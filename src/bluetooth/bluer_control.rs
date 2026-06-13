//! Real `bluer`-backed `BluetoothControl` — compile-checked D-Bus glue (B-2).
//!
//! No unit tests: this is real D-Bus (needs a running bluetoothd + adapter).
//! Validated live against the host Broadcom hci0; all other testing uses
//! `FakeBluetoothControl`.

use bluer::Session;

use crate::bluetooth::control::BluetoothControl;

/// Real Bluetooth adapter control backed by `bluer` (D-Bus / BlueZ).
pub struct BluerControl {
    adapter: bluer::Adapter,
}

impl BluerControl {
    /// Create a `BluerControl` bound to the system's default Bluetooth adapter.
    ///
    /// Opens a D-Bus session, then selects the default adapter (`hci0` when
    /// present, otherwise the lexicographically first adapter).
    pub async fn new() -> Result<Self, String> {
        let session = Session::new().await.map_err(|e| e.to_string())?;
        let adapter = session.default_adapter().await.map_err(|e| e.to_string())?;
        Ok(Self { adapter })
    }

    /// Build from an already-obtained adapter (shares the composition's session).
    pub fn with_adapter(adapter: bluer::Adapter) -> Self {
        Self { adapter }
    }

    /// HCI interface name of the adapter (e.g. `"hci0"`).
    ///
    /// Used to pass to `bluetooth::adapter::set_class_of_device` /
    /// `set_hci_name` which drive `hciconfig` for operations bluer can't do.
    pub fn hci_name(&self) -> String {
        self.adapter.name().to_string()
    }

    /// Power on the adapter and set its BlueZ alias (the name advertised to
    /// remote devices). AUD-003: alias is the friendly name shown by source
    /// firmware.
    pub async fn power_and_alias(&self, alias: &str) -> Result<(), String> {
        self.adapter
            .set_powered(true)
            .await
            .map_err(|e| e.to_string())?;
        self.set_alias(alias).await
    }

    /// Set only the BlueZ adapter alias (without powering on). Used for live
    /// rename while the adapter is already running.
    pub async fn set_alias(&self, name: &str) -> Result<(), String> {
        self.adapter
            .set_alias(name.to_string())
            .await
            .map_err(|e| e.to_string())
    }
}

impl BluetoothControl for BluerControl {
    /// Make the adapter discoverable (visible to scanning devices) or not.
    ///
    /// When turning discovery on, `DiscoverableTimeout` is also set to `0`
    /// (unlimited) so that the pairing-window controller owns the lifetime —
    /// BlueZ won't auto-cancel after the default 3-minute timeout.
    async fn set_discoverable(&self, on: bool) -> Result<(), String> {
        self.adapter
            .set_discoverable(on)
            .await
            .map_err(|e| e.to_string())?;
        // Timeout set after enabling: BlueZ resets the countdown on each property
        // write, so no window exists where the default 180 s timer could fire.
        if on {
            self.adapter
                .set_discoverable_timeout(0)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Make the adapter pairable or not.
    async fn set_pairable(&self, on: bool) -> Result<(), String> {
        self.adapter
            .set_pairable(on)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
