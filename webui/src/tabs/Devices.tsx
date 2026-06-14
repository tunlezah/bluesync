import { useState, useCallback, useEffect, useRef } from 'preact/hooks';
import { useStore } from '../lib/store';
import { startPairing, stopPairing, postDeviceCmd, isNotFound } from '../lib/api';
import { useToast } from '../components/Toast';
import type { DeviceInfo, DeviceState } from '../lib/types';
import './Devices.css';

// ── Helpers ────────────────────────────────────────────────────────────────

const DEFAULT_PAIR_SECONDS = 120;

function stateLabel(state: DeviceState): string {
  switch (state) {
    case 'audio_active': return 'Connected · audio active';
    case 'connected':    return 'Connected';
    case 'connecting':   return 'Connecting…';
    case 'discovered':   return 'Discovered';
    case 'disconnected': return 'Paired · not connected';
    default:             return state;
  }
}

function deviceDotClass(state: DeviceState): string {
  if (state === 'audio_active' || state === 'connected') return 'status-dot--active';
  if (state === 'connecting') return 'status-dot--connecting';
  return 'status-dot--inactive';
}

// Device icon is derived from the device's IDENTITY (BlueZ device class + name),
// NOT its connection state — so it is correct per-device AND "sticky": it stays
// the same when the device disconnects instead of reverting to a generic glyph.
// (Connection state is shown separately by the status dot.)
type DeviceKind = 'phone' | 'headphones' | 'speaker' | 'computer' | 'turntable' | 'cassette' | 'audio';

const KIND_ICON: Record<DeviceKind, string> = {
  phone:      '📱',
  headphones: '🎧',
  speaker:    '🔊',
  computer:   '💻',
  turntable:  '💿', // record player (e.g. Audio-Technica Sound Burger)
  cassette:   '📼', // cassette deck / Walkman
  audio:      '🎵',
};

// Strong name overrides for device types BlueZ can't distinguish (it reports a
// turntable/cassette as a generic "audio-card"). Checked before the BlueZ icon.
function nameOverrideKind(name: string | null): DeviceKind | null {
  const n = (name ?? '').toLowerCase();
  if (/sb-?727|sound ?burger|turntable|vinyl|record\b|phono/.test(n)) return 'turntable';
  if (/cassette|walkman|tape\b|tape ?deck|boombox|kxrm/.test(n))      return 'cassette';
  return null;
}

// BlueZ's own device-class icon hint — accurate for phones/headsets/computers.
function kindFromBluezIcon(icon: string | null): DeviceKind | null {
  switch (icon) {
    case 'phone':            return 'phone';
    case 'audio-headset':
    case 'audio-headphones': return 'headphones';
    case 'computer':         return 'computer';
    // 'audio-card' / 'multimedia-player' are too generic to tell a turntable
    // from a speaker — fall through to the name heuristic.
    default:                 return null;
  }
}

// Looser name heuristic, last resort before the generic glyph.
function looseNameKind(name: string | null): DeviceKind | null {
  const n = (name ?? '').toLowerCase();
  if (/iphone|ipad|ipod|pixel|galaxy|android|\bphone\b/.test(n))            return 'phone';
  if (/headphone|earbud|airpod|\bbuds\b|\bwh-|\bwf-|momentum|\bqc\d/.test(n)) return 'headphones';
  if (/speaker|soundbar|\bboom|\bflip\b|\bcharge\b|sonos|\bjbl\b|\bbose\b/.test(n)) return 'speaker';
  return null;
}

// Sticky cache of the best-known kind per device address. Survives reloads and
// fills in the icon if a device's name/icon is briefly absent while disconnected.
const KIND_CACHE_KEY = 'soundsync.deviceKinds.v1';

function loadKindCache(): Record<string, DeviceKind> {
  try { return JSON.parse(localStorage.getItem(KIND_CACHE_KEY) ?? '{}'); }
  catch { return {}; }
}

function rememberKind(addr: string, kind: DeviceKind): void {
  const cache = loadKindCache();
  if (cache[addr] === kind) return;
  cache[addr] = kind;
  try { localStorage.setItem(KIND_CACHE_KEY, JSON.stringify(cache)); } catch { /* ignore */ }
}

// Resolve a device to its kind, remembering it so the icon sticks across
// disconnects. Always returns a kind — falls back to the last-known cached kind,
// then a generic 'audio'.
function resolveKind(device: DeviceInfo): DeviceKind {
  const kind =
    nameOverrideKind(device.name)        // turntable / cassette (BlueZ can't tell)
    ?? kindFromBluezIcon(device.icon)    // phone / headphones / computer (accurate)
    ?? looseNameKind(device.name);       // backstop heuristic
  if (kind) {
    rememberKind(device.addr, kind);     // remember so it sticks across disconnects
    return kind;
  }
  return loadKindCache()[device.addr] ?? 'audio';
}

// The device glyph. Most kinds are an emoji; the turntable is a small inline SVG
// of a vinyl record, because no emoji reads as a record (💿/📀 look like CDs).
function DeviceIcon({ device }: { device: DeviceInfo }) {
  const kind = resolveKind(device);
  if (kind === 'turntable') {
    return (
      <svg class="device-icon-vinyl" viewBox="0 0 24 24" width="20" height="20" aria-hidden="true">
        <circle cx="12" cy="12" r="11" fill="#1e1e1e" stroke="#888" stroke-width="0.75" />
        <circle cx="12" cy="12" r="8" fill="none" stroke="#555" stroke-width="0.8" />
        <circle cx="12" cy="12" r="5.5" fill="#d2553a" />
        <circle cx="12" cy="12" r="1.3" fill="#1e1e1e" />
      </svg>
    );
  }
  return <>{KIND_ICON[kind]}</>;
}

// ── Pairing banner ─────────────────────────────────────────────────────────

interface PairingBannerProps {
  deviceName: string;
  durationSeconds: number;
  onStop: () => void;
  onExtend: () => void;
}

function PairingBanner({ deviceName, durationSeconds, onStop, onExtend }: PairingBannerProps) {
  const [remaining, setRemaining] = useState(durationSeconds);

  useEffect(() => {
    setRemaining(durationSeconds);
  }, [durationSeconds]);

  useEffect(() => {
    const interval = setInterval(() => {
      setRemaining(prev => {
        if (prev <= 1) {
          clearInterval(interval);
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
    return () => clearInterval(interval);
  }, [durationSeconds]);

  const mins = Math.floor(remaining / 60);
  const secs = remaining % 60;
  const timeStr = mins > 0 ? `${mins}:${secs.toString().padStart(2, '0')}` : `${secs}s`;

  return (
    <div class="card pairing-banner" role="status" aria-live="polite">
      <div class="pairing-banner__header">
        <span class="status-dot status-dot--connecting pairing-banner__dot" />
        <span class="pairing-banner__title">Discoverable as "{deviceName}"</span>
        {remaining > 0 && <span class="pairing-banner__timer">{timeStr}</span>}
        {remaining === 0 && <span class="pairing-banner__expired">Expired</span>}
      </div>
      <p class="pairing-banner__hint">
        Open your phone's Bluetooth settings and tap "{deviceName}" to pair.
      </p>
      <div class="pairing-banner__actions">
        <button class="btn btn-ghost pairing-banner__btn" onClick={onExtend}>
          Extend
        </button>
        <button class="btn btn-danger pairing-banner__btn" onClick={onStop}>
          Stop now
        </button>
      </div>
    </div>
  );
}

// ── Device row ─────────────────────────────────────────────────────────────

interface DeviceRowProps {
  device: DeviceInfo;
  deviceCmdsAvailable: boolean;
}

function DeviceRow({ device, deviceCmdsAvailable }: DeviceRowProps) {
  const { toast } = useToast();
  const [menuOpen, setMenuOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  const canConnect = device.state === 'disconnected' || device.state === 'discovered';
  const canDisconnect = device.state === 'connected' || device.state === 'audio_active';
  const name = device.name ?? device.addr;

  // Close menu on outside click
  useEffect(() => {
    if (!menuOpen) return;
    function handler(e: MouseEvent) {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    }
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [menuOpen]);

  const handleCmd = useCallback(async (cmd: 'connect' | 'disconnect' | 'forget') => {
    setMenuOpen(false);
    if (!deviceCmdsAvailable) {
      toast('Device commands coming soon', 'info');
      return;
    }
    setPending(true);
    try {
      const result = await postDeviceCmd(device.addr, cmd);
      if (result.ok) {
        toast(`${cmd.charAt(0).toUpperCase() + cmd.slice(1)} sent`, 'success');
      } else if (isNotFound(result)) {
        toast('Device commands coming soon', 'info');
      } else {
        toast(`${cmd} failed: ${result.message}`, 'error');
      }
    } finally {
      setPending(false);
    }
  }, [device.addr, deviceCmdsAvailable, toast]);

  return (
    <div class="device-row">
      <span class="device-row__icon" aria-hidden="true"><DeviceIcon device={device} /></span>
      <div class="device-row__info">
        <span class="device-row__name">{name}</span>
        <span class="device-row__state">{stateLabel(device.state)}</span>
      </div>
      <span class={`status-dot ${deviceDotClass(device.state)}`} aria-label={stateLabel(device.state)} />

      {canConnect && (
        <button
          class="btn btn-ghost device-row__connect"
          onClick={() => handleCmd('connect')}
          disabled={pending || !deviceCmdsAvailable}
          title={deviceCmdsAvailable ? 'Connect' : 'Coming soon'}
        >
          Connect
        </button>
      )}

      <div class="device-row__menu-wrap" ref={menuRef}>
        <button
          class="btn-icon device-row__menu-btn"
          onClick={() => setMenuOpen(v => !v)}
          aria-label={`Options for ${name}`}
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          disabled={pending}
        >
          ⋮
        </button>
        {menuOpen && (
          <div class="device-menu" role="menu">
            {canDisconnect && (
              <button
                class="device-menu__item"
                role="menuitem"
                onClick={() => handleCmd('disconnect')}
                title={deviceCmdsAvailable ? undefined : 'Coming soon'}
              >
                Disconnect
                {!deviceCmdsAvailable && <span class="device-menu__soon"> (soon)</span>}
              </button>
            )}
            <button
              class="device-menu__item device-menu__item--danger"
              role="menuitem"
              onClick={() => handleCmd('forget')}
              title={deviceCmdsAvailable ? undefined : 'Coming soon'}
            >
              Forget / Unpair
              {!deviceCmdsAvailable && <span class="device-menu__soon"> (soon)</span>}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Main tab ──────────────────────────────────────────────────────────────

export function DevicesTab() {
  const store = useStore();
  const { toast } = useToast();
  const [pairing, setPairing] = useState(false);
  const [pairDuration, setPairDuration] = useState(DEFAULT_PAIR_SECONDS);
  const [deviceCmdsAvailable, setDeviceCmdsAvailable] = useState(true);

  // Advertised device name from the store (live-updated via the `device_name` WS
  // message); falls back to 'SoundSync' only when unknown.
  const deviceName = store.deviceName || 'SoundSync';

  // Auto-close pairing banner when a device reaches connected/audio_active
  const prevDevicesRef = useRef(store.devices);
  useEffect(() => {
    if (!pairing) return;
    for (const dev of store.devices) {
      if ((dev.state === 'connected' || dev.state === 'audio_active') &&
          prevDevicesRef.current.find(d => d.addr === dev.addr)?.state !== dev.state) {
        // A device just connected — close the pairing banner
        setPairing(false);
        toast(`${dev.name ?? dev.addr} connected`, 'success');
      }
    }
    prevDevicesRef.current = store.devices;
  }, [store.devices, pairing, toast]);

  const handlePair = useCallback(async (seconds = DEFAULT_PAIR_SECONDS) => {
    const result = await startPairing(seconds);
    if (result.ok) {
      setPairing(true);
      setPairDuration(seconds);
    } else if (result.status === 503) {
      toast('Bluetooth not available', 'warning');
    } else {
      toast(`Pairing failed: ${result.message}`, 'error');
    }
  }, [toast]);

  const handleStopPairing = useCallback(async () => {
    const result = await stopPairing();
    setPairing(false);
    if (!result.ok && result.status !== 503) {
      toast(`Stop pairing failed: ${result.message}`, 'error');
    }
  }, [toast]);

  const handleExtend = useCallback(() => {
    handlePair(DEFAULT_PAIR_SECONDS);
  }, [handlePair]);

  const devices = store.devices;

  return (
    <div class="tab-content">
      {/* Pairing banner */}
      {pairing && (
        <PairingBanner
          deviceName={deviceName}
          durationSeconds={pairDuration}
          onStop={handleStopPairing}
          onExtend={handleExtend}
        />
      )}

      {/* Add device */}
      {!pairing && (
        <button
          class="btn btn-primary devices-pair-btn"
          onClick={() => handlePair()}
          aria-label="Pair a new Bluetooth device"
        >
          + Pair new device
        </button>
      )}

      {/* Device list */}
      <div class="card devices-list" aria-label="Bluetooth devices">
        {devices.length === 0 ? (
          <p class="devices-list__empty">No devices found yet.<br />Connect or pair a Bluetooth device.</p>
        ) : (
          devices.map(dev => (
            <DeviceRow
              key={dev.addr}
              device={dev}
              deviceCmdsAvailable={deviceCmdsAvailable}
            />
          ))
        )}
      </div>
    </div>
  );
}
