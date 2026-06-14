//! Outbound WebSocket message protocol (wire format) + event mapping.
use crate::state::{AppStateSnapshot, DeviceInfo, EqInfo, MediaInfo, OutputState, SystemEvent};
use serde::{Deserialize, Serialize};

/// SDP offer payload (browser -> server).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OfferData {
    pub sdp: String,
}

/// SDP answer payload (server -> browser).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AnswerData {
    pub sdp: String,
}

/// An ICE candidate, both directions. The key casing is load-bearing: Safari
/// requires exactly `sdpMid` / `sdpMLineIndex` (capital L) — explicit field
/// renames, NOT `rename_all = "camelCase"` (which yields `sdpMlineIndex`). AUD-048.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidate {
    pub candidate: String,
    #[serde(rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
}

/// Messages client -> server over `/ws/status` (WebRTC signalling).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsInMessage {
    WebrtcOffer { data: OfferData },
    WebrtcIceCandidate { data: IceCandidate },
    WebrtcStop,
}

/// Messages sent server -> client over `/ws/status`. Wire format: a `type` tag
/// in snake_case. Includes the WebRTC answer/ICE signalling (the inbound
/// counterparts are `WsInMessage`); the Safari `sdpMid`/`sdpMLineIndex` casing
/// is pinned on `IceCandidate` (AUD-048).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsOutMessage {
    StateSnapshot {
        data: Box<AppStateSnapshot>,
    },
    SpectrumData {
        bands: Vec<f32>,
    },
    BluetoothDevices {
        devices: Vec<DeviceInfo>,
    },
    WebrtcAnswer {
        data: AnswerData,
    },
    WebrtcIceCandidate {
        data: IceCandidate,
    },
    MediaState {
        media: Option<MediaInfo>,
    },
    EqState {
        eq: EqInfo,
    },
    /// Advertised Bluetooth device name changed.
    DeviceName {
        name: String,
    },
    /// Multi-output state changed (active device or available device list).
    OutputState {
        output: OutputState,
    },
}

/// Map an internal `SystemEvent` to its client-facing wire message, or `None`
/// if it is not forwarded to clients.
pub fn event_to_ws_message(event: &SystemEvent) -> Option<WsOutMessage> {
    match event {
        SystemEvent::SpectrumData { bands } => Some(WsOutMessage::SpectrumData {
            bands: bands.clone(),
        }),
        SystemEvent::BluetoothDevices { devices } => Some(WsOutMessage::BluetoothDevices {
            devices: devices.clone(),
        }),
        SystemEvent::MediaState { media } => Some(WsOutMessage::MediaState {
            media: media.clone(),
        }),
        SystemEvent::EqChanged { eq } => Some(WsOutMessage::EqState { eq: eq.clone() }),
        SystemEvent::DeviceNameChanged { name } => {
            Some(WsOutMessage::DeviceName { name: name.clone() })
        }
        SystemEvent::OutputState { output } => Some(WsOutMessage::OutputState {
            output: output.clone(),
        }),
        // Per-session webrtc events are routed via `webrtc_out_for_session`, not
        // the broadcast forwarder.
        SystemEvent::WebrtcAnswer { .. } | SystemEvent::WebrtcIceCandidate { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppStateSnapshot, DeviceInfo, SystemEvent, VERSION};

    #[test]
    fn state_snapshot_serializes_tagged() {
        let m = WsOutMessage::StateSnapshot {
            data: Box::new(AppStateSnapshot {
                version: VERSION.to_string(),
                bluetooth_devices: vec![],
                media: None,
                eq: crate::state::default_eq_info(),
                device_name: "SoundSync".to_string(),
                output: crate::state::OutputState::default(),
            }),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"state_snapshot\""));
        assert!(json.contains("\"data\":{"));
        assert!(json.contains("\"version\""));
    }

    #[test]
    fn bluetooth_devices_serializes_tagged() {
        let m = WsOutMessage::BluetoothDevices {
            devices: vec![DeviceInfo {
                addr: "AA:BB".to_string(),
                name: Some("Phone".to_string()),
                state: "connected".to_string(),
                icon: Some("phone".to_string()),
            }],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"bluetooth_devices\""));
        assert!(json.contains("\"addr\":\"AA:BB\""));
    }

    #[test]
    fn maps_bluetooth_devices_event() {
        let devices = vec![DeviceInfo {
            addr: "x".to_string(),
            name: None,
            state: "discovered".to_string(),
            icon: None,
        }];
        assert_eq!(
            event_to_ws_message(&SystemEvent::BluetoothDevices {
                devices: devices.clone()
            }),
            Some(WsOutMessage::BluetoothDevices { devices })
        );
    }

    #[test]
    fn spectrum_serializes_tagged() {
        let m = WsOutMessage::SpectrumData {
            bands: vec![0.5, 0.25],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "{\"type\":\"spectrum_data\",\"bands\":[0.5,0.25]}");
    }

    #[test]
    fn maps_system_events() {
        assert_eq!(
            event_to_ws_message(&SystemEvent::SpectrumData { bands: vec![1.0] }),
            Some(WsOutMessage::SpectrumData { bands: vec![1.0] })
        );
    }

    #[test]
    fn media_state_serializes_tagged() {
        let m = WsOutMessage::MediaState {
            media: Some(crate::state::MediaInfo {
                status: "playing".to_string(),
                title: Some("Song".to_string()),
                artist: None,
                album: None,
                duration_ms: None,
                position_ms: None,
                artwork_url: None,
            }),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"media_state\""));
        assert!(json.contains("\"status\":\"playing\""));
    }

    #[test]
    fn maps_media_state_event() {
        let media = Some(crate::state::MediaInfo {
            status: "paused".to_string(),
            title: None,
            artist: None,
            album: None,
            duration_ms: None,
            position_ms: None,
            artwork_url: None,
        });
        assert_eq!(
            event_to_ws_message(&SystemEvent::MediaState {
                media: media.clone()
            }),
            Some(WsOutMessage::MediaState { media })
        );
    }

    #[test]
    fn eq_state_serializes_tagged() {
        let m = WsOutMessage::EqState {
            eq: crate::state::default_eq_info(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"eq_state\""));
        assert!(json.contains("\"enabled\":false"));
    }

    #[test]
    fn maps_eq_changed_event() {
        let eq = crate::state::default_eq_info();
        let msg = event_to_ws_message(&SystemEvent::EqChanged { eq: eq.clone() });
        assert_eq!(msg, Some(WsOutMessage::EqState { eq }));
    }

    #[test]
    fn parses_inbound_offer() {
        let json = r#"{"type":"webrtc_offer","data":{"sdp":"v=0\r\n"}}"#;
        let m: WsInMessage = serde_json::from_str(json).unwrap();
        match m {
            WsInMessage::WebrtcOffer { data } => assert_eq!(data.sdp, "v=0\r\n"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parses_inbound_ice_with_exact_casing() {
        // The browser sends sdpMid / sdpMLineIndex (camelCase, capital L) — AUD-048.
        let json = r#"{"type":"webrtc_ice_candidate","data":{"candidate":"cand","sdpMid":"0","sdpMLineIndex":0}}"#;
        let m: WsInMessage = serde_json::from_str(json).unwrap();
        match m {
            WsInMessage::WebrtcIceCandidate { data } => {
                assert_eq!(data.candidate, "cand");
                assert_eq!(data.sdp_mid.as_deref(), Some("0"));
                assert_eq!(data.sdp_mline_index, Some(0));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parses_inbound_stop() {
        let m: WsInMessage = serde_json::from_str(r#"{"type":"webrtc_stop"}"#).unwrap();
        assert_eq!(m, WsInMessage::WebrtcStop);
    }

    #[test]
    fn outbound_ice_serializes_with_exact_casing() {
        let m = WsOutMessage::WebrtcIceCandidate {
            data: IceCandidate {
                candidate: "cand".to_string(),
                sdp_mid: Some("0".to_string()),
                sdp_mline_index: Some(0),
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"sdpMid\":\"0\""));
        // MUST be sdpMLineIndex (capital L), NOT sdpMlineIndex.
        assert!(json.contains("\"sdpMLineIndex\":0"));
        assert!(!json.contains("sdpMlineIndex"));
        assert!(json.contains("\"type\":\"webrtc_ice_candidate\""));
    }

    #[test]
    fn outbound_answer_serializes() {
        let m = WsOutMessage::WebrtcAnswer {
            data: AnswerData {
                sdp: "v=0".to_string(),
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"webrtc_answer\""));
        assert!(json.contains("\"sdp\":\"v=0\""));
    }

    #[test]
    fn device_name_serializes_tagged() {
        let m = WsOutMessage::DeviceName {
            name: "MyDevice".to_string(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"device_name\""));
        assert!(json.contains("\"name\":\"MyDevice\""));
    }

    #[test]
    fn maps_device_name_changed_event() {
        let msg = event_to_ws_message(&crate::state::SystemEvent::DeviceNameChanged {
            name: "Renamed".to_string(),
        });
        assert_eq!(
            msg,
            Some(WsOutMessage::DeviceName {
                name: "Renamed".to_string()
            })
        );
    }

    #[test]
    fn output_state_serializes_tagged() {
        let m = WsOutMessage::OutputState {
            output: crate::state::OutputState::default(),
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"output_state\""));
        assert!(json.contains("\"active\":null"));
        assert!(json.contains("\"available\":{"));
    }

    #[test]
    fn maps_output_state_event() {
        use crate::state::OutputState;
        let output = OutputState::default();
        let msg = event_to_ws_message(&crate::state::SystemEvent::OutputState {
            output: output.clone(),
        });
        assert_eq!(msg, Some(WsOutMessage::OutputState { output }));
    }

    #[test]
    fn output_state_serializes_cast_health_on_wire() {
        // NF-8: cast_health rides the wholesale OutputState clone into the WS
        // message and reaches the UI unchanged (snake_case, omitted when None).
        use crate::state::{CastHealth, OutputState};
        let m = WsOutMessage::OutputState {
            output: OutputState {
                active: None,
                available: crate::output::AvailableOutputs::default(),
                cast_health: Some(CastHealth::Lost),
            },
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"type\":\"output_state\""));
        assert!(json.contains("\"cast_health\":\"lost\""));
    }
}
