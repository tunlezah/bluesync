import { useState, useCallback } from 'preact/hooks';
import { useStore } from '../lib/store';
import { setOutput, setOutputNone, isUnavailable } from '../lib/api';
import { useToast } from './Toast';
import type { OutputDevice, OutputKind } from '../lib/types';
import './OutputPicker.css';

// ── Icons ─────────────────────────────────────────────────────────────────
// AirPlay and Cast use the canonical glyph shapes from the platform guidelines
// (Apple's AirPlay mark — rounded display + upward triangle; Google's Cast badge
// — screen with concentric broadcast arcs). The paths are the MIT-licensed
// Feather rendition of those marks, so we ship the recognizable official-style
// glyph as inline SVG without bundling Apple/Google brand assets.

function AirPlayIcon() {
  return (
    <svg
      class="output-glyph"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M6 17H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-2" />
      <path d="M12 15l5 6H7z" fill="currentColor" stroke="none" />
    </svg>
  );
}

function CastIcon() {
  return (
    <svg
      class="output-glyph"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.8"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M2 16.1A5 5 0 0 1 5.9 20M2 12.05A9 9 0 0 1 9.95 20M2 8V6a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-6" />
      <line x1="2" y1="20" x2="2.01" y2="20" />
    </svg>
  );
}

type TabKey = 'browser' | OutputKind;

function TabGlyph({ tab }: { tab: TabKey }) {
  switch (tab) {
    case 'airplay':
      return <AirPlayIcon />;
    case 'chromecast':
      return <CastIcon />;
    case 'soundcard':
      return <span class="output-emoji" aria-hidden="true">🔊</span>;
    case 'browser':
      return <span class="output-emoji" aria-hidden="true">💻</span>;
  }
}

const KIND_LABEL: Record<OutputKind, string> = {
  soundcard: 'Sound card',
  airplay: 'AirPlay',
  chromecast: 'Chromecast',
};

const TABS: { key: TabKey; label: string }[] = [
  { key: 'browser', label: 'This device' },
  { key: 'soundcard', label: 'Sound card' },
  { key: 'airplay', label: 'AirPlay' },
  { key: 'chromecast', label: 'Chromecast' },
];

// ── Active output display ─────────────────────────────────────────────────

interface ActiveBadgeProps {
  device: OutputDevice | null;
  switching: boolean;
}

function ActiveBadge({ device, switching }: ActiveBadgeProps) {
  if (switching) {
    return (
      <div class="output-active output-active--switching" aria-live="polite">
        <span class="spinner" aria-hidden="true" />
        <span>Switching…</span>
      </div>
    );
  }
  if (!device) {
    return (
      <div class="output-active output-active--none">
        <span class="output-active__icon">
          <TabGlyph tab="browser" />
        </span>
        <span class="output-active__label">This browser only</span>
      </div>
    );
  }
  return (
    <div class="output-active output-active--device">
      <span class="output-active__icon">
        <TabGlyph tab={device.kind} />
      </span>
      <span class="output-active__label">
        Playing to: <strong>{device.name}</strong>
      </span>
      <span class="output-active__kind">{KIND_LABEL[device.kind]}</span>
    </div>
  );
}

// ── Device row ───────────────────────────────────────────────────────────

interface DeviceRowProps {
  device: OutputDevice;
  isActive: boolean;
  isSwitching: boolean;
  onSelect: (kind: OutputKind, id: string) => void;
}

function DeviceRow({ device, isActive, isSwitching, onSelect }: DeviceRowProps) {
  return (
    <button
      class={`output-row ${isActive ? 'output-row--active' : ''}`}
      onClick={() => onSelect(device.kind, device.id)}
      disabled={isSwitching}
      aria-pressed={isActive}
      title={device.addr ?? undefined}
    >
      <span class="output-row__icon">
        <TabGlyph tab={device.kind} />
      </span>
      <span class="output-row__name">{device.name}</span>
      {isActive && (
        <span class="output-row__check" aria-hidden="true">
          ✓
        </span>
      )}
    </button>
  );
}

// ── Per-type panes ────────────────────────────────────────────────────────

interface DeviceListProps {
  kind: OutputKind;
  devices: OutputDevice[];
  activeId: string | null;
  isSwitching: boolean;
  onSelect: (kind: OutputKind, id: string) => void;
}

function DeviceList({ kind, devices, activeId, isSwitching, onSelect }: DeviceListProps) {
  return (
    <div class="output-list" role="group" aria-label={KIND_LABEL[kind]}>
      {kind === 'chromecast' && <p class="output-list__note">Chromecast support is in beta.</p>}
      {devices.length === 0 ? (
        <p class="output-list__empty">No {KIND_LABEL[kind]} devices found yet…</p>
      ) : (
        devices.map(d => (
          <DeviceRow
            key={d.id}
            device={d}
            isActive={d.id === activeId}
            isSwitching={isSwitching}
            onSelect={onSelect}
          />
        ))
      )}
    </div>
  );
}

interface BrowserPaneProps {
  isActive: boolean;
  isSwitching: boolean;
  onBrowserOnly: () => void;
}

function BrowserPane({ isActive, isSwitching, onBrowserOnly }: BrowserPaneProps) {
  return (
    <div class="output-list">
      <p class="output-list__note">Stops server output — listen here via the Listen button above.</p>
      <button
        class={`output-row ${isActive ? 'output-row--active' : ''}`}
        onClick={onBrowserOnly}
        disabled={isSwitching || isActive}
        aria-pressed={isActive}
      >
        <span class="output-row__icon">
          <TabGlyph tab="browser" />
        </span>
        <span class="output-row__name">Browser only{isActive ? '' : ' / Stop output'}</span>
        {isActive && (
          <span class="output-row__check" aria-hidden="true">
            ✓
          </span>
        )}
      </button>
    </div>
  );
}

// ── Main component ────────────────────────────────────────────────────────

export function OutputPicker() {
  const store = useStore();
  const { toast } = useToast();
  const [switching, setSwitching] = useState(false);

  const { output } = store;
  const active = output?.active ?? null;
  const available = output?.available ?? { soundcard: [], airplay: [], chromecast: [] };

  // Which category's list is shown — defaults to where audio is currently going.
  const [tab, setTab] = useState<TabKey>(active?.kind ?? 'browser');

  const counts: Record<OutputKind, number> = {
    soundcard: available.soundcard.length,
    airplay: available.airplay.length,
    chromecast: available.chromecast.length,
  };

  const handleSelect = useCallback(
    async (kind: OutputKind, id: string) => {
      // Skip if already active
      if (active?.kind === kind && active?.id === id) return;
      setSwitching(true);
      const result = await setOutput(kind, id);
      setSwitching(false);
      if (!result.ok) {
        if (isUnavailable(result)) {
          toast('Output unavailable — is the device reachable?', 'warning');
        } else if (result.status === 400) {
          toast('Invalid output selection', 'error');
        } else {
          toast(`Output error: ${result.message}`, 'error');
        }
      }
      // WS output_state event will confirm the change via store
    },
    [active, toast],
  );

  const handleBrowserOnly = useCallback(async () => {
    if (active === null) return; // already browser-only
    setSwitching(true);
    const result = await setOutputNone();
    setSwitching(false);
    if (!result.ok) {
      if (isUnavailable(result)) {
        toast('Output unavailable — is the server reachable?', 'warning');
      } else {
        toast(`Could not stop output: ${result.message}`, 'error');
      }
    }
  }, [active, toast]);

  return (
    <div class="output-picker">
      <div class="output-picker__header">
        <span class="output-picker__title">Output</span>
        <span class="output-picker__hint">Browser Listen always available alongside</span>
      </div>

      <ActiveBadge device={active} switching={switching} />

      {/* Segmented type filter — show one category at a time, never all at once */}
      <div class="output-segmented" role="tablist" aria-label="Output type">
        {TABS.map(t => (
          <button
            key={t.key}
            role="tab"
            aria-selected={tab === t.key}
            class={`output-seg ${tab === t.key ? 'output-seg--active' : ''}`}
            onClick={() => setTab(t.key)}
            title={t.label}
          >
            <span class="output-seg__icon">
              <TabGlyph tab={t.key} />
            </span>
            <span class="output-seg__label">{t.label}</span>
            {t.key !== 'browser' && counts[t.key] > 0 && (
              <span class="output-seg__count">{counts[t.key]}</span>
            )}
          </button>
        ))}
      </div>

      {/* Body — selected category only */}
      <div class="output-picker__body">
        {tab === 'browser' ? (
          <BrowserPane
            isActive={active === null}
            isSwitching={switching}
            onBrowserOnly={handleBrowserOnly}
          />
        ) : (
          <DeviceList
            kind={tab}
            devices={available[tab]}
            activeId={active?.kind === tab ? active.id : null}
            isSwitching={switching}
            onSelect={handleSelect}
          />
        )}
      </div>
    </div>
  );
}
