//! The narrow BlueZ control seam (real bluer impl lands in B-2; fake in `testing`).
use std::future::Future;

/// Adapter operations the Bluetooth logic needs. Static dispatch (RPITIT + Send),
/// like `ProcessSpawner` — no async-trait, no dyn. More methods (power, alias,
/// list/connect/disconnect, event stream) are added in later B slices.
pub trait BluetoothControl: Send + Sync {
    /// Make the adapter discoverable (visible to other devices) or not.
    fn set_discoverable(&self, on: bool) -> impl Future<Output = Result<(), String>> + Send;
    /// Make the adapter pairable or not.
    fn set_pairable(&self, on: bool) -> impl Future<Output = Result<(), String>> + Send;
}
