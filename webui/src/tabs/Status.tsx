import { useState, useCallback, useEffect } from 'preact/hooks';
import { useStore } from '../lib/store';
import { useTheme } from '../theme/ThemeContext';
import { postSettingsName, postAdminRestart, postAdminIdentify, isNotFound } from '../lib/api';
import { useToast } from '../components/Toast';
import { ACCENT_PRESETS } from '../theme/themes';
import type { AccentPreset, Appearance } from '../theme/ThemeContext';
import './Status.css';

// ── Health row helper ────────────────────────────────────────────────────

interface HealthRowProps {
  label: string;
  value: string;
  ok?: boolean | null;
}

function HealthRow({ label, value, ok }: HealthRowProps) {
  return (
    <div class="health-row">
      <span class="health-row__label">{label}</span>
      <span class="health-row__value">
        {ok !== undefined && ok !== null && (
          <span class={`status-dot ${ok ? 'status-dot--active' : 'status-dot--inactive'}`} />
        )}
        {value}
      </span>
    </div>
  );
}

// ── Stream link ──────────────────────────────────────────────────────────

interface StreamLinkProps {
  label: string;
  url: string;
}

function StreamLink({ label, url }: StreamLinkProps) {
  const { toast } = useToast();
  const fullUrl = `${window.location.origin}${url}`;

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(fullUrl);
      toast('URL copied', 'success');
    } catch {
      toast('Copy failed — try selecting manually', 'warning');
    }
  }, [fullUrl, toast]);

  return (
    <div class="stream-link">
      <span class="stream-link__label">{label}</span>
      <a href={url} class="stream-link__url" target="_blank" rel="noopener noreferrer" title={fullUrl}>
        {url}
      </a>
      <button class="btn btn-ghost stream-link__copy" onClick={handleCopy} aria-label={`Copy ${label} URL`}>
        Copy
      </button>
    </div>
  );
}

// ── Rename form ──────────────────────────────────────────────────────────

function RenameSection() {
  const { toast } = useToast();
  const store = useStore();
  const [name, setName] = useState(store.deviceName);
  const [pending, setPending] = useState(false);
  const [notAvailable, setNotAvailable] = useState(false);

  // Re-seed the input when the device name changes elsewhere (other tab,
  // reconnect snapshot, or a `device_name` WS push).
  useEffect(() => {
    setName(store.deviceName);
  }, [store.deviceName]);

  const handleRename = useCallback(async () => {
    if (!name.trim()) return;
    setPending(true);
    try {
      const result = await postSettingsName(name.trim());
      if (result.ok) {
        toast('Name updated', 'success');
      } else if (isNotFound(result)) {
        setNotAvailable(true);
        toast('Rename coming soon', 'info');
      } else {
        toast(`Rename failed: ${result.message}`, 'error');
      }
    } finally {
      setPending(false);
    }
  }, [name, toast]);

  return (
    <div class="settings-section">
      <h3 class="settings-section__title">Speaker Name</h3>
      {notAvailable ? (
        <p class="settings-section__soon">Rename coming soon (API not yet available)</p>
      ) : (
        <div class="rename-row">
          <input
            class="rename-input"
            type="text"
            value={name}
            onInput={(e) => setName((e.target as HTMLInputElement).value)}
            placeholder="SoundSync"
            maxLength={64}
            aria-label="Bluetooth device name"
            disabled={pending}
          />
          <button
            class="btn btn-primary rename-btn"
            onClick={handleRename}
            disabled={pending || !name.trim()}
          >
            {pending ? '…' : 'Save'}
          </button>
        </div>
      )}
    </div>
  );
}

// ── Admin section ─────────────────────────────────────────────────────────

function AdminSection() {
  const { toast } = useToast();
  const [notAvailable, setNotAvailable] = useState(false);

  const handle = useCallback(async (action: 'restart' | 'identify') => {
    const fn = action === 'restart' ? postAdminRestart : postAdminIdentify;
    const result = await fn();
    if (result.ok) {
      toast(`${action === 'restart' ? 'Restart' : 'Identify'} sent`, 'success');
    } else if (isNotFound(result)) {
      setNotAvailable(true);
      toast('Admin commands coming soon', 'info');
    } else {
      toast(`${action} failed: ${result.message}`, 'error');
    }
  }, [toast]);

  return (
    <div class="settings-section">
      <h3 class="settings-section__title">Admin</h3>
      {notAvailable ? (
        <p class="settings-section__soon">Admin actions coming soon</p>
      ) : (
        <div class="admin-actions">
          <button class="btn btn-ghost admin-btn" onClick={() => handle('restart')}>
            ↺ Restart audio engine
          </button>
          <button class="btn btn-ghost admin-btn" onClick={() => handle('identify')}>
            ● Identify
          </button>
        </div>
      )}
    </div>
  );
}

// ── Theme picker ──────────────────────────────────────────────────────────

const APPEARANCE_OPTIONS: { value: Appearance; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'dark',   label: 'Dark' },
  { value: 'light',  label: 'Light' },
];

function ThemePicker() {
  const { accent, appearance, setAccent, setAppearance } = useTheme();

  return (
    <div class="settings-section">
      <h3 class="settings-section__title">Theme</h3>
      <div class="theme-row">
        <span class="theme-row__label">Appearance</span>
        <div class="theme-seg" role="group" aria-label="Appearance mode">
          {APPEARANCE_OPTIONS.map(opt => (
            <button
              key={opt.value}
              class={`theme-seg__btn${appearance === opt.value ? ' theme-seg__btn--active' : ''}`}
              onClick={() => setAppearance(opt.value)}
              aria-pressed={appearance === opt.value}
            >
              {opt.label}
            </button>
          ))}
        </div>
      </div>
      <div class="theme-row">
        <span class="theme-row__label">Accent</span>
        <div class="accent-swatches" role="group" aria-label="Accent colour">
          {ACCENT_PRESETS.map(p => (
            <button
              key={p.id}
              class={`accent-swatch${accent === p.id ? ' accent-swatch--active' : ''}`}
              style={{ background: p.vars['--gradient-accent'] }}
              onClick={() => setAccent(p.id as AccentPreset)}
              title={p.label}
              aria-label={p.label}
              aria-pressed={accent === p.id}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

// ── Main tab ──────────────────────────────────────────────────────────────

export function StatusTab() {
  const store = useStore();
  const snapshot = store.snapshot;

  return (
    <div class="tab-content">
      {/* Health */}
      <div class="card">
        <h2 class="card-section-title">Health</h2>
        <div class="health-list">
          <HealthRow label="WebSocket" value={store.wsConnected ? 'Connected' : 'Reconnecting…'} ok={store.wsConnected} />
          <HealthRow label="Backend" value={snapshot?.version ?? '—'} />
          <HealthRow label="Interface" value={__APP_VERSION__} />
          {/* Future snapshot fields can be added here when backend extends AppStateSnapshot */}
        </div>
      </div>

      {/* Theme settings */}
      <div class="card">
        <h2 class="card-section-title">Appearance</h2>
        <ThemePicker />
      </div>

      {/* Stream links */}
      <div class="card">
        <h2 class="card-section-title">Stream Links</h2>
        <p class="stream-links__hint">
          Open in VLC or another media player for direct audio streaming.
        </p>
        <div class="stream-links-list">
          <StreamLink label="AAC stream" url="/api/stream/audio.aac" />
          <StreamLink label="MP3 stream" url="/api/stream/audio.mp3" />
        </div>
      </div>

      {/* Rename */}
      <div class="card">
        <h2 class="card-section-title">Device Name</h2>
        <RenameSection />
      </div>

      {/* Admin */}
      <div class="card">
        <h2 class="card-section-title">Admin</h2>
        <AdminSection />
      </div>
    </div>
  );
}
