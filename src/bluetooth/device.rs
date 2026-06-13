//! Device connection lifecycle (event-driven; see bluetooth-design.md §3).
//! `AudioActive` means the `bluez_input.*` PipeWire node exists (AUD-010) — the
//! real "audio is flowing" signal, not merely BlueZ "connected".

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Disconnected,
    Discovered,
    Connecting,
    Connected,
    AudioActive,
}

impl DeviceState {
    /// Stable snake_case wire name for the web UI.
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceState::Disconnected => "disconnected",
            DeviceState::Discovered => "discovered",
            DeviceState::Connecting => "connecting",
            DeviceState::Connected => "connected",
            DeviceState::AudioActive => "audio_active",
        }
    }

    /// Whether `next` is a valid transition from `self`. Disconnection is always
    /// allowed; otherwise only adjacent forward/backward steps in the lifecycle.
    pub fn can_transition_to(self, next: DeviceState) -> bool {
        use DeviceState::*;
        if next == Disconnected {
            return self != Disconnected;
        }
        matches!(
            (self, next),
            (Disconnected, Discovered)
                | (Discovered, Connecting)
                | (Connecting, Connected)
                | (Connected, AudioActive)
                | (AudioActive, Connected)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_strings_are_stable_snake_case() {
        assert_eq!(DeviceState::Disconnected.as_str(), "disconnected");
        assert_eq!(DeviceState::Discovered.as_str(), "discovered");
        assert_eq!(DeviceState::Connecting.as_str(), "connecting");
        assert_eq!(DeviceState::Connected.as_str(), "connected");
        assert_eq!(DeviceState::AudioActive.as_str(), "audio_active");
    }

    #[test]
    fn forward_progress_transitions_allowed() {
        assert!(DeviceState::Disconnected.can_transition_to(DeviceState::Discovered));
        assert!(DeviceState::Discovered.can_transition_to(DeviceState::Connecting));
        assert!(DeviceState::Connecting.can_transition_to(DeviceState::Connected));
        assert!(DeviceState::Connected.can_transition_to(DeviceState::AudioActive));
        assert!(DeviceState::AudioActive.can_transition_to(DeviceState::Connected));
        // node went away
    }

    #[test]
    fn disconnect_always_allowed() {
        for s in [
            DeviceState::Discovered,
            DeviceState::Connecting,
            DeviceState::Connected,
            DeviceState::AudioActive,
        ] {
            assert!(s.can_transition_to(DeviceState::Disconnected));
        }
    }

    #[test]
    fn skipping_states_is_rejected() {
        // Can't jump straight to AudioActive without being Connected.
        assert!(!DeviceState::Discovered.can_transition_to(DeviceState::AudioActive));
        assert!(!DeviceState::Disconnected.can_transition_to(DeviceState::AudioActive));
        // No-op self transition is not "progress".
        assert!(!DeviceState::Connected.can_transition_to(DeviceState::Connected));
    }
}
