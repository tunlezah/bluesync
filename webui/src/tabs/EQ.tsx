import { useState, useCallback, useEffect, useRef } from 'preact/hooks';
import { useStore } from '../lib/store';
import { getEq, postEq, isNotFound } from '../lib/api';
import { useToast } from '../components/Toast';
import type { EqInfo, EqBand } from '../lib/types';
import './EQ.css';

// ── EQ band configuration (legacy parity) ─────────────────────────────────

const BAND_LABELS = ['60', '120', '250', '500', '1k', '2k', '4k', '8k', '12k', '16k'];
const GAIN_MIN = -12;
const GAIN_MAX = 12;
const NUM_BANDS = 10;

// Built-in presets
interface Preset { name: string; gains: number[] }
const PRESETS: Preset[] = [
  { name: 'Flat',       gains: [0,0,0,0,0,0,0,0,0,0] },
  { name: 'Bass Boost', gains: [6,4,2,0,0,0,0,0,0,0] },
  { name: 'Treble Boost', gains: [0,0,0,0,0,0,2,4,5,6] },
  { name: 'Vocal',      gains: [-2,-1,0,2,4,4,2,0,-1,-2] },
  { name: 'Classical',  gains: [3,2,0,0,-1,0,0,2,3,3] },
  { name: 'Electronic', gains: [4,3,0,0,-2,0,3,4,4,4] },
];

// ── Frequency response SVG curve ──────────────────────────────────────────

function FreqCurve({ gains, enabled }: { gains: number[]; enabled: boolean }) {
  const W = 320;
  const H = 80;
  const PADDING = 12;
  const innerW = W - PADDING * 2;
  const innerH = H - PADDING * 2;
  const midY = PADDING + innerH / 2;

  // Map gains to y positions
  const points = gains.map((g, i) => {
    const x = PADDING + (i / (gains.length - 1)) * innerW;
    const y = midY - (g / GAIN_MAX) * (innerH / 2);
    return [x, y] as [number, number];
  });

  // Build smooth SVG path via cubic bezier
  function buildPath(pts: [number, number][]): string {
    if (pts.length < 2) return '';
    let d = `M ${pts[0][0]},${pts[0][1]}`;
    for (let i = 0; i < pts.length - 1; i++) {
      const [x1, y1] = pts[i];
      const [x2, y2] = pts[i + 1];
      const cpX = (x1 + x2) / 2;
      d += ` C ${cpX},${y1} ${cpX},${y2} ${x2},${y2}`;
    }
    return d;
  }

  const pathD = buildPath(points);
  const fillD = `${pathD} L ${points[points.length-1][0]},${H} L ${points[0][0]},${H} Z`;

  const opacity = enabled ? 1 : 0.35;

  return (
    <svg
      class="eq-curve"
      viewBox={`0 0 ${W} ${H}`}
      aria-hidden="true"
      style={{ opacity }}
    >
      {/* Zero line */}
      <line x1={PADDING} y1={midY} x2={W - PADDING} y2={midY} stroke="var(--color-border)" stroke-width="1" />
      {/* Fill */}
      <path d={fillD} fill="var(--color-accent)" opacity="0.15" />
      {/* Stroke */}
      <path d={pathD} fill="none" stroke="var(--color-accent)" stroke-width="2" stroke-linejoin="round" />
      {/* Control dots */}
      {points.map(([x, y], i) => (
        <circle key={i} cx={x} cy={y} r="3" fill="var(--color-accent)" />
      ))}
    </svg>
  );
}

// ── Vertical EQ slider ────────────────────────────────────────────────────

interface SliderProps {
  index: number;
  freq: string;
  gain: number;
  enabled: boolean;
  onChange: (index: number, gain: number) => void;
}

function EqSlider({ index, freq, gain, enabled, onChange }: SliderProps) {
  const handleChange = useCallback((e: Event) => {
    const v = parseFloat((e.target as HTMLInputElement).value);
    onChange(index, v);
  }, [index, onChange]);

  const gainDisplay = gain >= 0 ? `+${gain.toFixed(1)}` : gain.toFixed(1);

  return (
    <div class="eq-slider">
      <span class="eq-slider__gain" aria-live="polite">{gainDisplay}</span>
      <input
        type="range"
        class="eq-slider__track"
        // @ts-expect-error orient is a non-standard Firefox attribute for vertical range
        orient="vertical"
        min={GAIN_MIN}
        max={GAIN_MAX}
        step={0.5}
        value={gain}
        disabled={!enabled}
        onChange={handleChange}
        onInput={handleChange}
        aria-label={`${freq} Hz EQ band, ${gainDisplay} dB`}
        aria-valuemin={GAIN_MIN}
        aria-valuemax={GAIN_MAX}
        aria-valuenow={gain}
        aria-valuetext={`${gainDisplay} dB`}
      />
      <span class="eq-slider__freq">{freq}</span>
    </div>
  );
}

// ── Main EQ tab ───────────────────────────────────────────────────────────

export function EqTab() {
  const store = useStore();
  const { toast } = useToast();
  const [eqAvailable, setEqAvailable] = useState<boolean | null>(null); // null=loading
  const [enabled, setEnabled] = useState(true);
  const [gains, setGains] = useState<number[]>(new Array(NUM_BANDS).fill(0));
  const [presetsOpen, setPresetsOpen] = useState(false);
  const presetsRef = useRef<HTMLDivElement>(null);

  // Debounce ref for POST /api/eq
  const debounceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Ref so debounced callback always reads the live enabled value
  const enabledRef = useRef(enabled);
  useEffect(() => { enabledRef.current = enabled; }, [enabled]);

  // Load initial EQ state
  useEffect(() => {
    getEq().then(result => {
      if (result.ok) {
        setEqAvailable(true);
        setEnabled(result.data.enabled);
        setGains(result.data.bands.map(b => b.gain_db));
      } else if (isNotFound(result)) {
        setEqAvailable(false);
      } else {
        // Connection error — show as unavailable but don't permanently disable
        setEqAvailable(false);
        toast('Could not load EQ settings', 'warning');
      }
    });
  }, []);

  // Sync from WS eq_state events
  useEffect(() => {
    if (store.eq) {
      setEqAvailable(true);
      setEnabled(store.eq.enabled);
      setGains(store.eq.bands.map(b => b.gain_db));
    }
  }, [store.eq]);

  // Close presets dropdown on outside click
  useEffect(() => {
    if (!presetsOpen) return;
    function handler(e: MouseEvent) {
      if (presetsRef.current && !presetsRef.current.contains(e.target as Node)) {
        setPresetsOpen(false);
      }
    }
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [presetsOpen]);

  const sendGains = useCallback((newGains: number[], newEnabled?: boolean) => {
    if (debounceTimer.current) clearTimeout(debounceTimer.current);
    debounceTimer.current = setTimeout(async () => {
      const result = await postEq({
        enabled: newEnabled ?? enabledRef.current,
        gains: newGains,
      });
      if (!result.ok && !isNotFound(result)) {
        toast('EQ update failed', 'error');
      }
    }, 80); // 80ms debounce — live while dragging
  }, [toast]);

  const handleGainChange = useCallback((index: number, gain: number) => {
    const newGains = [...gains];
    newGains[index] = gain;
    setGains(newGains);
    sendGains(newGains);
  }, [gains, sendGains]);

  const handleToggle = useCallback(async () => {
    const newEnabled = !enabled;
    setEnabled(newEnabled);
    const result = await postEq({ enabled: newEnabled, gains });
    if (!result.ok && !isNotFound(result)) {
      setEnabled(enabled); // revert
      toast('EQ toggle failed', 'error');
    }
  }, [enabled, gains, toast]);

  const handleFlat = useCallback(() => {
    const newGains = new Array(NUM_BANDS).fill(0);
    setGains(newGains);
    sendGains(newGains);
  }, [sendGains]);

  const handlePreset = useCallback((preset: Preset) => {
    setPresetsOpen(false);
    setGains(preset.gains);
    sendGains(preset.gains);
  }, [sendGains]);

  // Loading state
  if (eqAvailable === null) {
    return (
      <div class="tab-content">
        <div class="card eq-loading">
          <div class="spinner" />
          <p>Loading EQ…</p>
        </div>
      </div>
    );
  }

  // Not available
  if (eqAvailable === false) {
    return (
      <div class="tab-content">
        <div class="card eq-unavailable">
          <p class="eq-unavailable__icon" aria-hidden="true">≈</p>
          <p class="eq-unavailable__title">EQ engine not available yet</p>
          <p class="eq-unavailable__hint">
            The equalizer backend is coming soon. Update SoundSync to enable it.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div class="tab-content eq-tab">
      {/* Header: toggle + Flat + Presets */}
      <div class="card eq-header">
        <label class="eq-toggle" aria-label="EQ enabled">
          <input
            type="checkbox"
            checked={enabled}
            onChange={handleToggle}
            class="eq-toggle__input"
          />
          <span class="eq-toggle__track" aria-hidden="true" />
          <span class="eq-toggle__label">{enabled ? 'On' : 'Bypass'}</span>
        </label>

        <div class="eq-header__actions">
          <button class="btn btn-ghost eq-action-btn" onClick={handleFlat} disabled={!enabled}>
            Flat
          </button>
          <div class="eq-presets-wrap" ref={presetsRef}>
            <button
              class="btn btn-ghost eq-action-btn"
              onClick={() => setPresetsOpen(v => !v)}
              disabled={!enabled}
              aria-haspopup="listbox"
              aria-expanded={presetsOpen}
            >
              Presets ▾
            </button>
            {presetsOpen && (
              <div class="eq-presets-menu" role="listbox" aria-label="EQ presets">
                {PRESETS.map(p => (
                  <button
                    key={p.name}
                    class="eq-presets-menu__item"
                    role="option"
                    onClick={() => handlePreset(p)}
                  >
                    {p.name}
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
      </div>

      {/* Frequency response curve */}
      <div class="card eq-curve-card">
        <FreqCurve gains={gains} enabled={enabled} />
      </div>

      {/* Sliders */}
      <div class={`card eq-sliders-card${!enabled ? ' eq-sliders-card--bypassed' : ''}`}>
        <div class="eq-sliders" role="group" aria-label="EQ band sliders">
          {BAND_LABELS.map((freq, i) => (
            <EqSlider
              key={freq}
              index={i}
              freq={freq}
              gain={gains[i] ?? 0}
              enabled={enabled}
              onChange={handleGainChange}
            />
          ))}
        </div>
        <div class="eq-sliders__scale" aria-hidden="true">
          <span>+{GAIN_MAX}</span>
          <span>0</span>
          <span>{GAIN_MIN}</span>
        </div>
      </div>
    </div>
  );
}
