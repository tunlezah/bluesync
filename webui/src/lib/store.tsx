import { createContext, ComponentChildren } from 'preact';
import { useContext, useEffect, useState, useCallback, useRef } from 'preact/hooks';
import { createWsClient, type WsClient } from './ws';
import type {
  AppStateSnapshot,
  DeviceInfo,
  MediaInfo,
  EqInfo,
  OutputState,
  WsServerMessage,
} from './types';

// ── Store state ─────────────────────────────────────────────────────────────

export interface AppStore {
  // Connection
  wsConnected: boolean;
  // Snapshot data
  snapshot: AppStateSnapshot | null;
  // Live-updated slices (patched by WS events)
  devices: DeviceInfo[];
  media: MediaInfo | null;
  eq: EqInfo | null;
  spectrum: number[]; // latest 64-band frame
  output: OutputState | null;
  deviceName: string;
  // Mutations
  setEq: (eq: EqInfo) => void;
  // WS client ref (for WebRTC signalling)
  ws: WsClient | null;
}

export const StoreContext = createContext<AppStore | null>(null);

export function useStore(): AppStore {
  const ctx = useContext(StoreContext);
  if (!ctx) throw new Error('useStore must be used inside StoreProvider');
  return ctx;
}

interface Props { children: ComponentChildren }

export function StoreProvider({ children }: Props) {
  const [wsConnected, setWsConnected] = useState(false);
  const [snapshot, setSnapshot] = useState<AppStateSnapshot | null>(null);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [media, setMedia] = useState<MediaInfo | null>(null);
  const [eq, setEq] = useState<EqInfo | null>(null);
  const [spectrum, setSpectrum] = useState<number[]>([]);
  const [output, setOutput] = useState<OutputState | null>(null);
  const [deviceName, setDeviceName] = useState('SoundSync');
  const wsRef = useRef<WsClient | null>(null);
  const [wsInstance, setWsInstance] = useState<WsClient | null>(null);

  useEffect(() => {
    const client = createWsClient('/ws/status');
    wsRef.current = client;
    setWsInstance(client);

    const unsubStatus = client.onStatusChange(setWsConnected);

    const unsubMsg = client.onMessage((msg: WsServerMessage) => {
      switch (msg.type) {
        case 'state_snapshot': {
          setSnapshot(msg.data);
          setDevices(msg.data.bluetooth_devices);
          setMedia(msg.data.media);
          if (msg.data.eq) setEq(msg.data.eq);
          if (msg.data.output) setOutput(msg.data.output);
          if (msg.data.device_name) setDeviceName(msg.data.device_name);
          break;
        }
        case 'bluetooth_devices':
          setDevices(msg.devices);
          break;
        case 'media_state':
          setMedia(msg.media);
          break;
        case 'eq_state':
          setEq(msg.eq);
          break;
        case 'spectrum_data':
          setSpectrum(msg.bands);
          break;
        case 'output_state':
          setOutput(msg.output);
          break;
        case 'device_name':
          setDeviceName(msg.name);
          break;
        // webrtc_answer and webrtc_ice_candidate are handled by the webrtc module
        default:
          break;
      }
    });

    return () => {
      unsubStatus();
      unsubMsg();
      client.close();
      wsRef.current = null;
    };
  }, []);

  const handleSetEq = useCallback((newEq: EqInfo) => {
    setEq(newEq);
  }, []);

  const store: AppStore = {
    wsConnected,
    snapshot,
    devices,
    media,
    eq,
    spectrum,
    output,
    deviceName,
    setEq: handleSetEq,
    ws: wsInstance,
  };

  return <StoreContext.Provider value={store}>{children}</StoreContext.Provider>;
}
