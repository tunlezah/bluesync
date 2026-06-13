import type { AccentPreset, Appearance } from './ThemeContext';

export interface AccentTheme {
  id: AccentPreset;
  label: string;
  vars: Record<string, string>;
}

// ── Accent presets ──────────────────────────────────────────────────────────

export const ACCENT_PRESETS: AccentTheme[] = [
  {
    id: 'indigo-cyan',
    label: 'Indigo · Cyan',
    vars: {
      '--color-accent':       '#6366f1',
      '--color-accent-hover': '#4f46e5',
      '--color-accent-dim':   'rgba(99,102,241,0.15)',
      '--color-accent2':      '#06b6d4',
      '--gradient-accent':    'linear-gradient(135deg, #6366f1 0%, #06b6d4 100%)',
    },
  },
  {
    id: 'hifi-amber',
    label: 'Hi-Fi Amber',
    vars: {
      '--color-accent':       '#f59e0b',
      '--color-accent-hover': '#d97706',
      '--color-accent-dim':   'rgba(245,158,11,0.15)',
      '--color-accent2':      '#ef4444',
      '--gradient-accent':    'linear-gradient(135deg, #f59e0b 0%, #ef4444 100%)',
    },
  },
  {
    id: 'minimal-teal',
    label: 'Minimal Teal',
    vars: {
      '--color-accent':       '#14b8a6',
      '--color-accent-hover': '#0d9488',
      '--color-accent-dim':   'rgba(20,184,166,0.15)',
      '--color-accent2':      '#64748b',
      '--gradient-accent':    'linear-gradient(135deg, #14b8a6 0%, #64748b 100%)',
    },
  },
  {
    id: 'slate-blue',
    label: 'Slate Blue',
    vars: {
      '--color-accent':       '#5468a8',
      '--color-accent-hover': '#43528a',
      '--color-accent-dim':   'rgba(84,104,168,0.15)',
      '--color-accent2':      '#7c93cf',
      '--gradient-accent':    'linear-gradient(135deg, #5468a8 0%, #7c93cf 100%)',
    },
  },
  {
    id: 'royal-magenta',
    label: 'Royal Magenta',
    vars: {
      '--color-accent':       '#9333ea',
      '--color-accent-hover': '#7e22ce',
      '--color-accent-dim':   'rgba(147,51,234,0.15)',
      '--color-accent2':      '#ec4899',
      '--gradient-accent':    'linear-gradient(135deg, #9333ea 0%, #ec4899 100%)',
    },
  },
  {
    id: 'forest-lime',
    label: 'Forest Lime',
    vars: {
      '--color-accent':       '#157f3c',
      '--color-accent-hover': '#166534',
      '--color-accent-dim':   'rgba(21,127,60,0.15)',
      '--color-accent2':      '#84cc16',
      '--gradient-accent':    'linear-gradient(135deg, #157f3c 0%, #84cc16 100%)',
    },
  },
  {
    id: 'desert-red',
    label: 'Desert Red',
    vars: {
      '--color-accent':       '#b04d33',
      '--color-accent-hover': '#8a3a26',
      '--color-accent-dim':   'rgba(176,77,51,0.15)',
      '--color-accent2':      '#e08a6f',
      '--gradient-accent':    'linear-gradient(135deg, #b04d33 0%, #e08a6f 100%)',
    },
  },
];

// ── Dark / Light palette variables ──────────────────────────────────────────

const DARK_PALETTE: Record<string, string> = {
  '--color-bg':           '#0d0e14',
  '--color-bg-elevated':  '#12131a',
  '--color-card':         'rgba(255,255,255,0.04)',
  '--color-card-hover':   'rgba(255,255,255,0.07)',
  '--color-card-active':  'rgba(255,255,255,0.10)',
  '--color-border':       'rgba(255,255,255,0.10)',
  '--color-text':         '#e8eaf0',
  '--color-text-muted':   '#9ca3af',
  '--color-text-dim':     '#6b7280',
  '--color-success':      '#22c55e',
  '--color-warning':      '#f59e0b',
  '--color-danger':       '#ef4444',
  '--gradient-bg':        'radial-gradient(ellipse at 50% 0%, rgba(99,102,241,0.12) 0%, transparent 60%)',
};

const LIGHT_PALETTE: Record<string, string> = {
  '--color-bg':           '#f1f2f6',
  '--color-bg-elevated':  '#ffffff',
  '--color-card':         'rgba(255,255,255,0.85)',
  '--color-card-hover':   'rgba(255,255,255,0.95)',
  '--color-card-active':  'rgba(220,224,235,0.9)',
  '--color-border':       'rgba(0,0,0,0.10)',
  '--color-text':         '#1a1b22',
  '--color-text-muted':   '#6b7280',
  '--color-text-dim':     '#9ca3af',
  '--color-success':      '#16a34a',
  '--color-warning':      '#d97706',
  '--color-danger':       '#dc2626',
  '--gradient-bg':        'radial-gradient(ellipse at 50% 0%, rgba(99,102,241,0.07) 0%, transparent 60%)',
};

// ── Shared / structural variables ────────────────────────────────────────────

const STRUCTURAL_VARS: Record<string, string> = {
  '--font-body':     "'Inter', 'Segoe UI', system-ui, -apple-system, sans-serif",
  '--font-mono':     "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
  '--radius-sm':     '6px',
  '--radius-md':     '10px',
  '--radius-lg':     '16px',
  '--radius-xl':     '24px',
  '--spacing-xs':    '6px',
  '--spacing-sm':    '10px',
  '--spacing-md':    '16px',
  '--spacing-lg':    '24px',
  '--spacing-xl':    '40px',
  '--tab-height':    '56px',
  '--max-content':   '640px',
  '--transition-ui': '0.2s ease',
};

// ── Apply helpers ─────────────────────────────────────────────────────────────

export function applyVars(vars: Record<string, string>, root: HTMLElement = document.documentElement): void {
  for (const [k, v] of Object.entries(vars)) {
    root.style.setProperty(k, v);
  }
}

/** Resolve the actual color scheme (dark|light) from the appearance setting */
export function resolveColorScheme(appearance: Appearance): 'dark' | 'light' {
  if (appearance === 'system') {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  }
  return appearance;
}

export function applyTheme(accent: AccentPreset, appearance: Appearance, root?: HTMLElement): void {
  const el = root ?? document.documentElement;
  const scheme = resolveColorScheme(appearance);
  const palette = scheme === 'dark' ? DARK_PALETTE : LIGHT_PALETTE;
  const accentTheme = ACCENT_PRESETS.find(p => p.id === accent) ?? ACCENT_PRESETS[0];

  applyVars(STRUCTURAL_VARS, el);
  applyVars(palette, el);
  applyVars(accentTheme.vars, el);

  // Also update gradient-bg to use current accent (override after palette)
  el.style.setProperty('--gradient-bg',
    `radial-gradient(ellipse at 50% 0%, ${accentTheme.vars['--color-accent-dim']} 0%, transparent 60%)`
  );

  el.setAttribute('data-scheme', scheme);
  el.setAttribute('data-accent', accent);
}
