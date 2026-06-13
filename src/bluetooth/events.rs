//! Device lifecycle events → `DeviceState` transitions (bluetooth-design.md §3/§4).
//! Events come from BlueZ device signals (B-3b) and the `bluez_input.*` node
//! probe (AUD-010). The reducer is pure: it maps each event to a target state and
//! applies it only if `DeviceState::can_transition_to` allows, otherwise the
//! state is unchanged (illegal/stale events are dropped, never panic).
use crate::bluetooth::device::DeviceState;

/// An observed Bluetooth device lifecycle event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceEvent {
    /// Device showed up in discovery.
    Discovered,
    /// A connection attempt began.
    Connecting,
    /// BlueZ reports the device connected (transport not necessarily up).
    Connected,
    /// The `bluez_input.*` PipeWire node appeared — audio is flowing (AUD-010).
    AudioNodeAppeared,
    /// The `bluez_input.*` node went away (still connected, but no audio).
    AudioNodeDisappeared,
    /// Device disconnected (or removed).
    Disconnected,
}

impl DeviceEvent {
    /// The state this event drives toward.
    fn target(self) -> DeviceState {
        match self {
            DeviceEvent::Discovered => DeviceState::Discovered,
            DeviceEvent::Connecting => DeviceState::Connecting,
            // AudioNodeDisappeared → Connected is a deliberate backward step: BlueZ re-reports
            // "connected" after a transient node drop, so AudioActive→Connected must be legal.
            DeviceEvent::Connected | DeviceEvent::AudioNodeDisappeared => DeviceState::Connected,
            DeviceEvent::AudioNodeAppeared => DeviceState::AudioActive,
            DeviceEvent::Disconnected => DeviceState::Disconnected,
        }
    }
}

/// Apply `event` to `state`, returning the new state. Valid transitions advance
/// the state; invalid/stale events (and no-op self transitions) leave it
/// unchanged. Disconnection from any active state is always honoured.
pub fn apply_event(state: DeviceState, event: DeviceEvent) -> DeviceState {
    let target = event.target();
    if target == state || state.can_transition_to(target) {
        target
    } else {
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluetooth::device::DeviceState;

    #[test]
    fn full_connect_flow_reaches_audio_active() {
        let mut s = DeviceState::Disconnected;
        for (ev, expect) in [
            (DeviceEvent::Discovered, DeviceState::Discovered),
            (DeviceEvent::Connecting, DeviceState::Connecting),
            (DeviceEvent::Connected, DeviceState::Connected),
            (DeviceEvent::AudioNodeAppeared, DeviceState::AudioActive),
        ] {
            s = apply_event(s, ev);
            assert_eq!(s, expect);
        }
    }

    #[test]
    fn node_disappearing_drops_to_connected() {
        let s = apply_event(DeviceState::AudioActive, DeviceEvent::AudioNodeDisappeared);
        assert_eq!(s, DeviceState::Connected);
    }

    #[test]
    fn disconnect_from_any_active_state_goes_disconnected() {
        for start in [
            DeviceState::Discovered,
            DeviceState::Connecting,
            DeviceState::Connected,
            DeviceState::AudioActive,
        ] {
            assert_eq!(
                apply_event(start, DeviceEvent::Disconnected),
                DeviceState::Disconnected
            );
        }
    }

    #[test]
    fn illegal_transition_is_ignored_state_unchanged() {
        // AudioNodeAppeared while only Discovered is not a valid jump -> no change.
        assert_eq!(
            apply_event(DeviceState::Discovered, DeviceEvent::AudioNodeAppeared),
            DeviceState::Discovered
        );
        // Disconnect while already Disconnected stays Disconnected.
        assert_eq!(
            apply_event(DeviceState::Disconnected, DeviceEvent::Disconnected),
            DeviceState::Disconnected
        );
    }

    #[test]
    fn redundant_connected_event_is_noop() {
        assert_eq!(
            apply_event(DeviceState::Connected, DeviceEvent::Connected),
            DeviceState::Connected
        );
    }
}
