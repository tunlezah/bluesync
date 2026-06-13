// Wire contract types — mirror the Rust DTOs exactly

export type DeviceState =
  | 'disconnected'
  | 'discovered'
  | 'connecting'
  | 'connected'
  | 'audio_active';

export interface DeviceInfo {
  addr: string;
  name: string | null;
  state: DeviceState;
  /** BlueZ `Icon` hint (e.g. "phone", "audio-card", "audio-headset"); null if unknown. */
  icon: string | null;
}

export type MediaStatus = 'stopped' | 'playing' | 'paused';

export interface MediaInfo {
  status: MediaStatus;
  title: string | null;
  artist: string | null;
  album: string | null;
  duration_ms: number | null;
  position_ms: number | null;
  /** Best-effort album art URL from the iTunes Search API; null until fetched or when no match. */
  artwork_url: string | null;
}

export interface EqBand {
  freq: number;
  gain_db: number;
  q: number;
  filter_type: 'low_shelf' | 'peaking' | 'high_shelf';
}

export interface EqInfo {
  enabled: boolean;
  bands: EqBand[];
}

// ── Output types ──────────────────────────────────────────────────────────

export type OutputKind = 'soundcard' | 'airplay' | 'chromecast';

export interface OutputDevice {
  kind: OutputKind;
  id: string;
  name: string;
  addr?: string | null;
  port?: number | null;
}

export interface AvailableOutputs {
  soundcard: OutputDevice[];
  airplay: OutputDevice[];
  chromecast: OutputDevice[];
}

export interface OutputState {
  active: OutputDevice | null;
  available: AvailableOutputs;
}

export interface AppStateSnapshot {
  version: string;
  bluetooth_devices: DeviceInfo[];
  media: MediaInfo | null;
  eq?: EqInfo;
  output?: OutputState;
}

// IceCandidate — exact camelCase keys required (load-bearing for Safari)
export interface IceCandidate {
  candidate: string;
  sdpMid: string | null;
  sdpMLineIndex: number | null;
}

// WebSocket message types — server → client
export type WsServerMessage =
  | { type: 'state_snapshot'; data: AppStateSnapshot }
  | { type: 'spectrum_data'; bands: number[] }
  | { type: 'bluetooth_devices'; devices: DeviceInfo[] }
  | { type: 'media_state'; media: MediaInfo | null }
  | { type: 'eq_state'; eq: EqInfo }
  | { type: 'output_state'; output: OutputState }
  | { type: 'webrtc_answer'; data: { sdp: string } }
  | { type: 'webrtc_ice_candidate'; data: IceCandidate };

// WebSocket message types — client → server
export type WsClientMessage =
  | { type: 'webrtc_offer'; data: { sdp: string } }
  | { type: 'webrtc_ice_candidate'; data: IceCandidate }
  | { type: 'webrtc_stop' };
