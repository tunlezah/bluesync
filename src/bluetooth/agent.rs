//! The pairing auto-accept gate (bluetooth-design.md §2/§4). This is the
//! security-critical decision: the real BlueZ `Agent1` (B-3b) auto-accepts a
//! pairing/authorization request ONLY while the pairing window is open. The
//! window's open/closed state is published on a `watch` channel by
//! `run_pairing_window`; the gate reads it. Replacing legacy's always-on
//! auto-accept honours the hardened-defaults decision.
use tokio::sync::watch;

/// Consults the pairing-window `open_state` to decide whether to auto-accept an
/// incoming pairing/authorization request. Cheap to clone the receiver into the
/// agent callbacks.
#[derive(Clone)]
pub struct PairingGate {
    open: watch::Receiver<bool>,
}

impl PairingGate {
    /// Build a gate over the pairing window's `open_state` receiver.
    pub fn new(open: watch::Receiver<bool>) -> Self {
        Self { open }
    }

    /// `true` iff the pairing window is currently open — the only condition under
    /// which an incoming pairing request is auto-accepted.
    #[must_use = "ignoring the gate result means all pairing requests are unconditionally accepted"]
    pub fn authorized(&self) -> bool {
        *self.open.borrow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::watch;

    #[test]
    fn rejects_when_window_closed() {
        let (_tx, rx) = watch::channel(false);
        let gate = PairingGate::new(rx);
        assert!(!gate.authorized());
    }

    #[test]
    fn accepts_when_window_open() {
        let (_tx, rx) = watch::channel(true);
        let gate = PairingGate::new(rx);
        assert!(gate.authorized());
    }

    #[test]
    fn reflects_window_state_changes() {
        let (tx, rx) = watch::channel(false);
        let gate = PairingGate::new(rx);
        assert!(!gate.authorized());
        tx.send(true).unwrap();
        assert!(gate.authorized());
        tx.send(false).unwrap();
        assert!(!gate.authorized());
    }
}
