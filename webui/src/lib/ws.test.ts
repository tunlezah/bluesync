import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import type { WsServerMessage } from './types';

// ── WS message parsing tests ─────────────────────────────────────────────────
// We test the message type discriminant logic directly (the client dispatches
// by msg.type — these tests verify the shape contracts are correct).

describe('WS message type discrimination', () => {
  it('state_snapshot has .data with bluetooth_devices', () => {
    const msg: WsServerMessage = {
      type: 'state_snapshot',
      data: {
        version: '0.1.0',
        bluetooth_devices: [{ addr: 'AA:BB:CC:DD:EE:FF', name: 'Phone', state: 'audio_active', icon: 'phone' }],
        media: null,
      },
    };
    expect(msg.type).toBe('state_snapshot');
    if (msg.type === 'state_snapshot') {
      expect(msg.data.bluetooth_devices).toHaveLength(1);
      expect(msg.data.bluetooth_devices[0].addr).toBe('AA:BB:CC:DD:EE:FF');
    }
  });

  it('spectrum_data has .bands array', () => {
    const bands = Array.from({ length: 64 }, (_, i) => i / 64);
    const msg: WsServerMessage = { type: 'spectrum_data', bands };
    expect(msg.type).toBe('spectrum_data');
    if (msg.type === 'spectrum_data') {
      expect(msg.bands).toHaveLength(64);
    }
  });

  it('bluetooth_devices has .devices array', () => {
    const msg: WsServerMessage = {
      type: 'bluetooth_devices',
      devices: [{ addr: '11:22:33:44:55:66', name: null, state: 'disconnected', icon: null }],
    };
    if (msg.type === 'bluetooth_devices') {
      expect(msg.devices[0].state).toBe('disconnected');
    }
  });

  it('media_state has .media field (nullable)', () => {
    const msg: WsServerMessage = { type: 'media_state', media: null };
    if (msg.type === 'media_state') {
      expect(msg.media).toBeNull();
    }
  });

  it('eq_state has .eq with enabled + bands', () => {
    const msg: WsServerMessage = {
      type: 'eq_state',
      eq: {
        enabled: true,
        bands: [{ freq: 60, gain_db: 3.5, q: 0.7, filter_type: 'low_shelf' }],
      },
    };
    if (msg.type === 'eq_state') {
      expect(msg.eq.enabled).toBe(true);
      expect(msg.eq.bands[0].filter_type).toBe('low_shelf');
    }
  });

  it('device_name has .name string', () => {
    const msg: WsServerMessage = { type: 'device_name', name: 'Living Room' };
    expect(msg.type).toBe('device_name');
    if (msg.type === 'device_name') {
      expect(msg.name).toBe('Living Room');
    }
  });

  it('webrtc_answer has .data.sdp', () => {
    const msg: WsServerMessage = { type: 'webrtc_answer', data: { sdp: 'v=0\r\n...' } };
    if (msg.type === 'webrtc_answer') {
      expect(msg.data.sdp).toContain('v=0');
    }
  });

  it('webrtc_ice_candidate preserves exact camelCase keys (sdpMid/sdpMLineIndex)', () => {
    const msg: WsServerMessage = {
      type: 'webrtc_ice_candidate',
      data: { candidate: 'candidate:0 1 UDP ...', sdpMid: 'audio', sdpMLineIndex: 0 },
    };
    if (msg.type === 'webrtc_ice_candidate') {
      // These are the exact keys the server sends — camelCase is load-bearing for Safari
      expect(msg.data.sdpMid).toBe('audio');
      expect(msg.data.sdpMLineIndex).toBe(0);
    }
  });
});

// ── Store mapping logic (isolated, without Preact render) ─────────────────────

interface MockState {
  snapshot: unknown;
  devices: unknown[];
  media: unknown;
  eq: unknown;
  spectrum: number[];
  deviceName: string;
  output: import('./types').OutputState | null;
}

function applyMessage(state: MockState, msg: WsServerMessage): MockState {
  switch (msg.type) {
    case 'state_snapshot':
      return {
        ...state,
        snapshot: msg.data,
        devices: msg.data.bluetooth_devices,
        media: msg.data.media,
        eq: msg.data.eq ?? state.eq,
        deviceName: msg.data.device_name ?? state.deviceName,
        output: msg.data.output ?? state.output,
      };
    case 'bluetooth_devices':
      return { ...state, devices: msg.devices };
    case 'media_state':
      return { ...state, media: msg.media };
    case 'eq_state':
      return { ...state, eq: msg.eq };
    case 'spectrum_data':
      return { ...state, spectrum: msg.bands };
    case 'device_name':
      return { ...state, deviceName: msg.name };
    case 'output_state':
      return { ...state, output: msg.output };
    default:
      return state;
  }
}

describe('store state transitions', () => {
  const initial: MockState = { snapshot: null, devices: [], media: null, eq: null, spectrum: [], deviceName: 'SoundSync', output: null };

  it('state_snapshot seeds all slices', () => {
    const msg: WsServerMessage = {
      type: 'state_snapshot',
      data: {
        version: '0.1.0',
        bluetooth_devices: [{ addr: 'AA:BB:CC:DD:EE:FF', name: 'Test', state: 'connected', icon: null }],
        media: { status: 'playing', title: 'Song', artist: 'Artist', album: null, duration_ms: 180000, position_ms: 30000, artwork_url: null },
      },
    };
    const next = applyMessage(initial, msg);
    expect(next.devices).toHaveLength(1);
    expect((next.media as { title: string }).title).toBe('Song');
  });

  it('bluetooth_devices replaces devices slice', () => {
    const msg: WsServerMessage = {
      type: 'bluetooth_devices',
      devices: [{ addr: '11:22:33:44:55:66', name: 'Headphones', state: 'audio_active', icon: 'audio-headset' }],
    };
    const next = applyMessage({ ...initial, devices: [] }, msg);
    expect(next.devices).toHaveLength(1);
  });

  it('media_state sets media to null', () => {
    const msg: WsServerMessage = { type: 'media_state', media: null };
    const next = applyMessage({ ...initial, media: { status: 'playing' } as unknown }, msg);
    expect(next.media).toBeNull();
  });

  it('eq_state updates eq slice', () => {
    const msg: WsServerMessage = {
      type: 'eq_state',
      eq: { enabled: false, bands: [] },
    };
    const next = applyMessage(initial, msg);
    expect((next.eq as { enabled: boolean }).enabled).toBe(false);
  });

  it('spectrum_data updates spectrum slice', () => {
    const bands = new Array(64).fill(0.5);
    const msg: WsServerMessage = { type: 'spectrum_data', bands };
    const next = applyMessage(initial, msg);
    expect(next.spectrum).toHaveLength(64);
    expect(next.spectrum[0]).toBe(0.5);
  });

  it('device_name updates deviceName slice', () => {
    const msg: WsServerMessage = { type: 'device_name', name: 'Kitchen Speaker' };
    const next = applyMessage(initial, msg);
    expect(next.deviceName).toBe('Kitchen Speaker');
  });

  it('output_state with cast_health=lost clears the active device (NF-8)', () => {
    // First a connecting cast becomes active.
    const connecting: WsServerMessage = {
      type: 'output_state',
      output: {
        active: { kind: 'chromecast', id: 'cc1', name: 'Living Room' },
        available: { soundcard: [], airplay: [], chromecast: [] },
        cast_health: 'connecting',
      },
    };
    let next = applyMessage(initial, connecting);
    expect(next.output?.active?.id).toBe('cc1');
    expect(next.output?.cast_health).toBe('connecting');

    // Then the session dies: backend clears active and reports lost.
    const lost: WsServerMessage = {
      type: 'output_state',
      output: {
        active: null,
        available: { soundcard: [], airplay: [], chromecast: [] },
        cast_health: 'lost',
      },
    };
    next = applyMessage(next, lost);
    expect(next.output?.active).toBeNull();
    expect(next.output?.cast_health).toBe('lost');
  });

  it('output_state without cast_health is accepted (backward compatible)', () => {
    const msg: WsServerMessage = {
      type: 'output_state',
      output: {
        active: null,
        available: { soundcard: [], airplay: [], chromecast: [] },
      },
    };
    const next = applyMessage(initial, msg);
    expect(next.output?.active).toBeNull();
    expect(next.output?.cast_health ?? null).toBeNull();
  });

  it('state_snapshot seeds deviceName when present', () => {
    const msg: WsServerMessage = {
      type: 'state_snapshot',
      data: {
        version: '0.1.0',
        bluetooth_devices: [],
        media: null,
        device_name: 'Bedroom',
      },
    };
    const next = applyMessage(initial, msg);
    expect(next.deviceName).toBe('Bedroom');
  });
});
