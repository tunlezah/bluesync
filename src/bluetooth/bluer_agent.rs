//! BlueZ Agent1 registration. The box is headless, so it registers a
//! **`NoInputNoOutput`** agent → BlueZ uses "Just Works" pairing and **never**
//! shows a passkey/confirmation code.
//!
//! Access is gated by the pairing WINDOW via the adapter's `Pairable` property:
//! `run_pairing_window` sets `pairable`/`discoverable` true only while the window
//! is open (and re-asserts false when closed, recovering from adapter resets), so
//! BlueZ refuses new pairings outside the window. We deliberately do NOT gate in
//! the agent: per `bluer`, providing *any* `request_authorization` /
//! `request_confirmation` / `authorize_service` callback makes the agent advertise
//! `DisplayYesNo`, which forces numeric-comparison pairing (a 6-digit code). For a
//! display-less appliance that is wrong — the window/`Pairable` is the boundary.
//!
//! The returned `AgentHandle` must outlive the manager (drop = agent unregistered).
use crate::bluetooth::agent::PairingGate;

/// Register a headless (`NoInputNoOutput`) pairing agent on `session`: BlueZ does
/// codeless "Just Works" pairing, gated by the pairing window's `Pairable`
/// toggling rather than by an agent callback. Returns the `AgentHandle`; keep it
/// alive to keep the agent registered.
///
/// `_gate` is retained for call-site compatibility but intentionally unused — the
/// window (`Pairable`/`Discoverable`) is the access gate now, because any agent
/// authorization callback would force a `DisplayYesNo` code.
pub async fn register_pairing_agent(
    session: &bluer::Session,
    _gate: PairingGate,
) -> Result<bluer::agent::AgentHandle, String> {
    // All callbacks `None` ⇒ `NoInputNoOutput` capability ⇒ Just Works, no code.
    let agent = bluer::agent::Agent::default();
    session
        .register_agent(agent)
        .await
        .map_err(|e| e.to_string())
}
