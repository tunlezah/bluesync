//! Shared runtime state + the internal event bus (web-webrtc-design.md §4).
use crate::dsp::eq::{default_bands, EqBand, FilterType, NUM_BANDS};
use crate::output::{AvailableOutputs, OutputDevice};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

/// App version (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Now-playing media (AVRCP) as shown to the UI. `status` is `PlaybackStatus::as_str`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MediaInfo {
    pub status: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u32>,
    pub position_ms: Option<u32>,
    /// Best-effort album art URL fetched from the iTunes Search API.
    /// `None` until the artwork lookup completes, or when no art is found.
    pub artwork_url: Option<String>,
}

/// A Bluetooth device as shown to the UI. `state` is `DeviceState::as_str`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub addr: String,
    pub name: Option<String>,
    pub state: String,
    /// BlueZ `Icon` hint (e.g. "phone", "audio-card", "audio-headset"); the UI
    /// maps it to a device-type glyph. `None` if BlueZ reported no icon.
    pub icon: Option<String>,
}

/// A single EQ band as shown to the UI (read-only freq/q/type; mutable gain_db).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EqBandInfo {
    pub freq: f64,
    pub gain_db: f32,
    pub q: f64,
    pub filter_type: String,
}

/// EQ state as shown to the UI.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EqInfo {
    pub enabled: bool,
    pub bands: Vec<EqBandInfo>,
}

/// Health of an active Chromecast session, surfaced to the UI (NF-8).
///
/// Two-state by design:
/// - `Connecting` — a cast session has been started and its task is alive.
/// - `Lost` — the session task died (EOF / read-error / write-failure / the
///   NF-26 inbound deadline); the controller has cleared the active output.
///
/// There is deliberately NO `Live` variant: confirming "playing" would require
/// a liveness channel from the session task back to the controller, which is
/// out of scope.  `Connecting` covers the alive-but-unconfirmed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CastHealth {
    Connecting,
    Lost,
}

/// Output subsystem state as shown to the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct OutputState {
    /// The currently active output device, or `None` (browser-only mode).
    pub active: Option<OutputDevice>,
    /// All currently discovered output devices.
    pub available: AvailableOutputs,
    /// Health of the active Chromecast session, if any (NF-8).  `None` when no
    /// cast is in play.  Additive + optional so the wire format and all
    /// by-field construction sites stay backward-compatible.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cast_health: Option<CastHealth>,
}

/// Map a `FilterType` to the wire string.
fn filter_type_str(ft: FilterType) -> &'static str {
    match ft {
        FilterType::LowShelf => "low_shelf",
        FilterType::Peaking => "peaking",
        FilterType::HighShelf => "high_shelf",
    }
}

/// Convert DSP bands + enabled flag to the UI DTO.
pub fn eq_info(enabled: bool, bands: &[EqBand]) -> EqInfo {
    EqInfo {
        enabled,
        bands: bands
            .iter()
            .map(|b| EqBandInfo {
                freq: b.freq,
                gain_db: b.gain_db,
                q: b.q,
                filter_type: filter_type_str(b.filter_type).to_string(),
            })
            .collect(),
    }
}

/// Build the default `EqInfo` (disabled, all gains 0).
pub fn default_eq_info() -> EqInfo {
    eq_info(false, &default_bands())
}

/// Mutable runtime state. Grows as subsystems land; minimal for now.
#[derive(Debug)]
pub struct AppState {
    pub bluetooth_devices: Vec<DeviceInfo>,
    pub media: Option<MediaInfo>,
    pub eq: EqInfo,
    /// Advertised Bluetooth device name.
    pub device_name: String,
    /// Multi-output subsystem state.
    pub output: OutputState,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            bluetooth_devices: Vec::new(),
            media: None,
            eq: default_eq_info(),
            device_name: "SoundSync".to_string(),
            output: OutputState::default(),
        }
    }
}

/// Serializable state view sent to new WS clients and `GET /api/status`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AppStateSnapshot {
    pub version: String,
    pub bluetooth_devices: Vec<DeviceInfo>,
    pub media: Option<MediaInfo>,
    pub eq: EqInfo,
    /// Advertised Bluetooth device name.
    pub device_name: String,
    /// Multi-output subsystem state.
    pub output: OutputState,
}

/// Internal event-bus messages: subsystems publish, the WS forwarder consumes.
#[derive(Debug, Clone, PartialEq)]
pub enum SystemEvent {
    SpectrumData {
        bands: Vec<f32>,
    },
    /// WebRTC SDP answer for a session (server -> that browser).
    WebrtcAnswer {
        session: uuid::Uuid,
        sdp: String,
    },
    /// WebRTC ICE candidate for a session.
    WebrtcIceCandidate {
        session: uuid::Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
    /// The current Bluetooth device list changed (server -> all clients).
    BluetoothDevices {
        devices: Vec<DeviceInfo>,
    },
    /// Now-playing media changed (server -> all clients). `None` = no player.
    MediaState {
        media: Option<MediaInfo>,
    },
    /// EQ state changed (server -> all clients).
    EqChanged {
        eq: EqInfo,
    },
    /// The advertised Bluetooth device name changed.
    DeviceNameChanged {
        name: String,
    },
    /// Multi-output state changed (active device or available device list).
    OutputState {
        output: OutputState,
    },
}

/// Suppresses the unused-import warning when NUM_BANDS is only used in tests.
const _: usize = NUM_BANDS;

/// Cloneable handle to shared state + the event bus.
#[derive(Clone)]
pub struct AppStateHandle {
    pub state: Arc<RwLock<AppState>>,
    events: broadcast::Sender<SystemEvent>,
}

impl AppStateHandle {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            state: Arc::new(RwLock::new(AppState::default())),
            events,
        }
    }

    /// Subscribe to the event bus.
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.events.subscribe()
    }

    /// Publish an event (ignores the no-subscribers case).
    pub fn publish(&self, event: SystemEvent) {
        let _ = self.events.send(event);
    }

    /// Current serializable snapshot.
    pub async fn snapshot(&self) -> AppStateSnapshot {
        let state = self.state.read().await;
        AppStateSnapshot {
            version: VERSION.to_string(),
            bluetooth_devices: state.bluetooth_devices.clone(),
            media: state.media.clone(),
            eq: state.eq.clone(),
            device_name: state.device_name.clone(),
            output: state.output.clone(),
        }
    }

    /// Update the advertised Bluetooth device name and publish the change.
    pub async fn set_device_name(&self, name: String) {
        {
            let mut state = self.state.write().await;
            state.device_name = name.clone();
        }
        self.publish(SystemEvent::DeviceNameChanged { name });
    }

    /// Replace the now-playing media and publish the change.
    pub async fn set_media(&self, media: Option<MediaInfo>) {
        {
            let mut state = self.state.write().await;
            state.media = media.clone();
        }
        self.publish(SystemEvent::MediaState { media });
    }

    /// Replace the Bluetooth device list and publish the change.
    pub async fn set_bluetooth_devices(&self, devices: Vec<DeviceInfo>) {
        {
            let mut state = self.state.write().await;
            state.bluetooth_devices = devices.clone();
        }
        self.publish(SystemEvent::BluetoothDevices { devices });
    }

    /// Replace the EQ state and publish the change.
    pub async fn set_eq(&self, eq: EqInfo) {
        {
            let mut state = self.state.write().await;
            state.eq = eq.clone();
        }
        self.publish(SystemEvent::EqChanged { eq });
    }

    /// Replace the output state and publish the change.
    pub async fn set_output(&self, output: OutputState) {
        {
            let mut state = self.state.write().await;
            state.output = output.clone();
        }
        self.publish(SystemEvent::OutputState { output });
    }
}

impl Default for AppStateHandle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_reports_version() {
        let h = AppStateHandle::new();
        assert_eq!(h.snapshot().await.version, VERSION);
    }

    #[tokio::test]
    async fn publish_reaches_a_subscriber() {
        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        h.publish(SystemEvent::SpectrumData {
            bands: vec![0.1, 0.2],
        });
        if let SystemEvent::SpectrumData { bands } = rx.recv().await.unwrap() {
            assert_eq!(bands, vec![0.1, 0.2]);
        } else {
            panic!("expected spectrum data");
        }
    }

    #[test]
    fn publish_without_subscribers_is_ok() {
        let h = AppStateHandle::new();
        h.publish(SystemEvent::SpectrumData { bands: vec![] }); // must not panic
    }

    #[test]
    fn device_info_serializes_snake_case() {
        let d = DeviceInfo {
            addr: "AA:BB:CC:DD:EE:FF".to_string(),
            name: Some("Phone".to_string()),
            state: "audio_active".to_string(),
            icon: Some("phone".to_string()),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"addr\":\"AA:BB:CC:DD:EE:FF\""));
        assert!(json.contains("\"name\":\"Phone\""));
        assert!(json.contains("\"state\":\"audio_active\""));
        assert!(json.contains("\"icon\":\"phone\""));
    }

    #[test]
    fn media_info_serializes_with_optionals() {
        let m = MediaInfo {
            status: "playing".to_string(),
            title: Some("Song".to_string()),
            artist: Some("Artist".to_string()),
            album: None,
            duration_ms: Some(210000),
            position_ms: Some(1000),
            artwork_url: None,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"status\":\"playing\""));
        assert!(json.contains("\"title\":\"Song\""));
        assert!(json.contains("\"album\":null"));
        assert!(json.contains("\"duration_ms\":210000"));
    }

    #[tokio::test]
    async fn set_media_updates_snapshot_and_publishes() {
        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        let media = Some(MediaInfo {
            status: "paused".to_string(),
            title: None,
            artist: None,
            album: None,
            duration_ms: None,
            position_ms: None,
            artwork_url: None,
        });
        h.set_media(media.clone()).await;
        assert_eq!(h.snapshot().await.media, media);
        match rx.recv().await.unwrap() {
            SystemEvent::MediaState { media: m } => assert_eq!(m, media),
            other => panic!("expected MediaState, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_bluetooth_devices_updates_snapshot_and_publishes() {
        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        let devices = vec![DeviceInfo {
            addr: "x".to_string(),
            name: None,
            state: "connected".to_string(),
            icon: None,
        }];
        h.set_bluetooth_devices(devices.clone()).await;
        // snapshot reflects it
        assert_eq!(h.snapshot().await.bluetooth_devices, devices);
        // and it was published
        match rx.recv().await.unwrap() {
            SystemEvent::BluetoothDevices { devices: d } => assert_eq!(d, devices),
            other => panic!("expected BluetoothDevices, got {other:?}"),
        }
    }

    #[test]
    fn default_eq_info_shape() {
        let info = default_eq_info();
        assert!(!info.enabled);
        assert_eq!(info.bands.len(), NUM_BANDS);
        assert!(info.bands.iter().all(|b| b.gain_db == 0.0));
    }

    #[test]
    fn filter_type_strings_are_correct() {
        let bands = default_bands();
        let info = eq_info(false, &bands);
        assert_eq!(info.bands[0].filter_type, "low_shelf");
        assert_eq!(info.bands[1].filter_type, "peaking");
        assert_eq!(info.bands[9].filter_type, "high_shelf");
        // all middle bands are peaking
        for b in &info.bands[1..9] {
            assert_eq!(b.filter_type, "peaking");
        }
    }

    #[test]
    fn eq_info_serializes_snake_case_fields() {
        let info = default_eq_info();
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"enabled\":false"));
        assert!(json.contains("\"gain_db\":"));
        assert!(json.contains("\"filter_type\":"));
        assert!(json.contains("\"freq\":"));
        assert!(json.contains("\"low_shelf\""));
    }

    #[tokio::test]
    async fn set_device_name_updates_snapshot_and_publishes() {
        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        h.set_device_name("MyDevice".to_string()).await;
        assert_eq!(h.snapshot().await.device_name, "MyDevice");
        match rx.recv().await.unwrap() {
            SystemEvent::DeviceNameChanged { name } => assert_eq!(name, "MyDevice"),
            other => panic!("expected DeviceNameChanged, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_device_name_is_soundsync() {
        let h = AppStateHandle::new();
        assert_eq!(h.snapshot().await.device_name, "SoundSync");
    }

    #[tokio::test]
    async fn set_eq_updates_snapshot_and_publishes() {
        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        let eq = EqInfo {
            enabled: true,
            bands: default_eq_info().bands,
        };
        h.set_eq(eq.clone()).await;
        assert_eq!(h.snapshot().await.eq, eq);
        match rx.recv().await.unwrap() {
            SystemEvent::EqChanged { eq: e } => assert_eq!(e, eq),
            other => panic!("expected EqChanged, got {other:?}"),
        }
    }

    #[test]
    fn output_state_default_is_empty() {
        let s = OutputState::default();
        assert!(s.active.is_none());
        assert!(s.available.soundcard.is_empty());
        assert!(s.available.airplay.is_empty());
        assert!(s.available.chromecast.is_empty());
    }

    #[test]
    fn output_state_serializes_with_active_null() {
        let s = OutputState::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"active\":null"));
        assert!(json.contains("\"available\":{"));
        assert!(json.contains("\"soundcard\":[]"));
        assert!(json.contains("\"airplay\":[]"));
        assert!(json.contains("\"chromecast\":[]"));
    }

    #[test]
    fn output_state_omits_cast_health_when_none() {
        // skip_serializing_if keeps the wire format backward-compatible: the
        // field is absent entirely when there is no active cast.
        let s = OutputState::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("cast_health"));
    }

    #[test]
    fn output_state_serializes_cast_health_snake_case() {
        let s = OutputState {
            active: None,
            available: AvailableOutputs::default(),
            cast_health: Some(CastHealth::Connecting),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"cast_health\":\"connecting\""));

        let s = OutputState {
            active: None,
            available: AvailableOutputs::default(),
            cast_health: Some(CastHealth::Lost),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"cast_health\":\"lost\""));
    }

    #[tokio::test]
    async fn set_output_updates_snapshot_and_publishes() {
        use crate::output::{AvailableOutputs, OutputDevice, OutputKind};

        let h = AppStateHandle::new();
        let mut rx = h.subscribe();
        let device = OutputDevice {
            kind: OutputKind::Soundcard,
            id: "alsa_output.test".to_string(),
            name: "Test Speaker".to_string(),
            addr: None,
            port: None,
        };
        let output = OutputState {
            active: Some(device),
            available: AvailableOutputs::default(),
            cast_health: None,
        };
        h.set_output(output.clone()).await;
        assert_eq!(h.snapshot().await.output, output);
        match rx.recv().await.unwrap() {
            SystemEvent::OutputState { output: o } => assert_eq!(o, output),
            other => panic!("expected OutputState, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_snapshot_has_no_active_output() {
        let h = AppStateHandle::new();
        assert!(h.snapshot().await.output.active.is_none());
    }
}
