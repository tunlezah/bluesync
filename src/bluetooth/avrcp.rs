//! AVRCP core (bluetooth-design.md §6, AUD-011): media playback status, track
//! metadata, transport commands, and the D-Bus staleness counter. Pure logic;
//! the real `org.bluez.MediaPlayer1` D-Bus wiring + reconnect loop is B-4b.

/// Playback status (a stable wire subset of BlueZ `MediaPlayer1.Status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
}

impl PlaybackStatus {
    /// Stable snake_case wire name for the web UI.
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackStatus::Stopped => "stopped",
            PlaybackStatus::Playing => "playing",
            PlaybackStatus::Paused => "paused",
        }
    }

    /// Map a BlueZ `Status` string. `playing`/`paused` map directly; everything
    /// else (`stopped`, `forward-seek`, `reverse-seek`, `error`, unknown)
    /// collapses to `Stopped` (conservative — no false "playing").
    pub fn from_bluez(s: &str) -> Self {
        match s {
            "playing" => PlaybackStatus::Playing,
            "paused" => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        }
    }
}

/// Track metadata from `MediaPlayer1.Track`. Artwork is intentionally omitted
/// (UB-4: legacy artwork is non-functional; a future to-build feature).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
}

/// BlueZ reports a missing/unknown duration as `0` or `0xFFFFFFFF`; both mean
/// "no duration" — map them to `None` so the UI doesn't show a sentinel.
pub fn sanitize_duration_ms(raw: u32) -> Option<u32> {
    if raw == 0 || raw == u32::MAX {
        None
    } else {
        Some(raw)
    }
}

/// Trim a metadata string; an empty/whitespace-only value (BlueZ sends `""` for
/// an absent field) becomes `None`.
pub fn sanitize_text(s: String) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// A transport control the UI can issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCommand {
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

impl TransportCommand {
    /// The `org.bluez.MediaPlayer1` method name for this command.
    pub fn bluez_method(self) -> &'static str {
        match self {
            TransportCommand::Play => "Play",
            TransportCommand::Pause => "Pause",
            TransportCommand::Next => "Next",
            TransportCommand::Previous => "Previous",
            TransportCommand::Stop => "Stop",
        }
    }

    /// Parse a web action token (`/api/media/<action>`) into a command.
    pub fn from_action(action: &str) -> Option<Self> {
        match action {
            "play" => Some(TransportCommand::Play),
            "pause" => Some(TransportCommand::Pause),
            "next" => Some(TransportCommand::Next),
            "previous" => Some(TransportCommand::Previous),
            "stop" => Some(TransportCommand::Stop),
            _ => None,
        }
    }
}

/// Consecutive-failure threshold before the D-Bus connection is torn down and
/// re-established (AUD-011: long-running connections silently go stale after
/// adapter reset / suspend-resume / dbus restart).
pub const DBUS_FAILURE_THRESHOLD: u32 = 5;

/// Tracks consecutive D-Bus failures for the staleness-reconnect pattern.
#[derive(Debug, Default)]
pub struct DbusHealth {
    consecutive_failures: u32,
}

impl DbusHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// A successful call resets the failure streak.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
    }

    /// A failed call. Returns `true` when the threshold is reached (the caller
    /// should reconnect); the counter resets when it fires so the next streak
    /// is counted fresh.
    pub fn record_failure(&mut self) -> bool {
        self.consecutive_failures += 1;
        if self.consecutive_failures >= DBUS_FAILURE_THRESHOLD {
            self.consecutive_failures = 0;
            true
        } else {
            false
        }
    }

    /// Current consecutive-failure count (inspection/tests).
    pub fn failures(&self) -> u32 {
        self.consecutive_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_status_wire_strings() {
        assert_eq!(PlaybackStatus::Stopped.as_str(), "stopped");
        assert_eq!(PlaybackStatus::Playing.as_str(), "playing");
        assert_eq!(PlaybackStatus::Paused.as_str(), "paused");
    }

    #[test]
    fn playback_status_from_bluez() {
        assert_eq!(
            PlaybackStatus::from_bluez("playing"),
            PlaybackStatus::Playing
        );
        assert_eq!(PlaybackStatus::from_bluez("paused"), PlaybackStatus::Paused);
        assert_eq!(
            PlaybackStatus::from_bluez("stopped"),
            PlaybackStatus::Stopped
        );
        // Unknown / seek / error states collapse to Stopped (conservative).
        assert_eq!(
            PlaybackStatus::from_bluez("forward-seek"),
            PlaybackStatus::Stopped
        );
        assert_eq!(PlaybackStatus::from_bluez("error"), PlaybackStatus::Stopped);
    }

    #[test]
    fn transport_command_bluez_method_names() {
        assert_eq!(TransportCommand::Play.bluez_method(), "Play");
        assert_eq!(TransportCommand::Pause.bluez_method(), "Pause");
        assert_eq!(TransportCommand::Next.bluez_method(), "Next");
        assert_eq!(TransportCommand::Previous.bluez_method(), "Previous");
        assert_eq!(TransportCommand::Stop.bluez_method(), "Stop");
    }

    #[test]
    fn transport_command_from_action() {
        assert_eq!(
            TransportCommand::from_action("play"),
            Some(TransportCommand::Play)
        );
        assert_eq!(
            TransportCommand::from_action("pause"),
            Some(TransportCommand::Pause)
        );
        assert_eq!(
            TransportCommand::from_action("next"),
            Some(TransportCommand::Next)
        );
        assert_eq!(
            TransportCommand::from_action("previous"),
            Some(TransportCommand::Previous)
        );
        assert_eq!(
            TransportCommand::from_action("stop"),
            Some(TransportCommand::Stop)
        );
        assert_eq!(TransportCommand::from_action("eject"), None);
    }

    #[test]
    fn dbus_health_resets_on_success() {
        let mut h = DbusHealth::new();
        assert!(!h.record_failure());
        assert!(!h.record_failure());
        assert_eq!(h.failures(), 2);
        h.record_success();
        assert_eq!(h.failures(), 0);
    }

    #[test]
    fn duration_sentinels_map_to_none() {
        assert_eq!(sanitize_duration_ms(0), None);
        assert_eq!(sanitize_duration_ms(u32::MAX), None);
        assert_eq!(sanitize_duration_ms(210_000), Some(210_000));
    }

    #[test]
    fn blank_text_maps_to_none() {
        assert_eq!(sanitize_text(String::new()), None);
        assert_eq!(sanitize_text("   ".to_string()), None);
        assert_eq!(sanitize_text("Song".to_string()), Some("Song".to_string()));
    }

    #[test]
    fn dbus_health_fires_at_threshold_and_resets() {
        let mut h = DbusHealth::new();
        // 4 failures: not yet.
        for _ in 0..(DBUS_FAILURE_THRESHOLD - 1) {
            assert!(!h.record_failure());
        }
        // 5th failure fires the reconnect signal and resets the counter.
        assert!(h.record_failure());
        assert_eq!(h.failures(), 0);
    }
}
