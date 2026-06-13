import { describe, it, expect, vi, beforeEach } from 'vitest';
import { resolveColorScheme, applyTheme, ACCENT_PRESETS } from './themes';

// Mock matchMedia
function mockMatchMedia(prefersDark: boolean) {
  vi.stubGlobal('matchMedia', (query: string) => ({
    matches: query === '(prefers-color-scheme: dark)' ? prefersDark : false,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  }));
}

describe('resolveColorScheme', () => {
  it('returns "dark" when appearance is "dark"', () => {
    expect(resolveColorScheme('dark')).toBe('dark');
  });

  it('returns "light" when appearance is "light"', () => {
    expect(resolveColorScheme('light')).toBe('light');
  });

  it('follows system preference when appearance is "system" — dark', () => {
    mockMatchMedia(true);
    expect(resolveColorScheme('system')).toBe('dark');
  });

  it('follows system preference when appearance is "system" — light', () => {
    mockMatchMedia(false);
    expect(resolveColorScheme('system')).toBe('light');
  });
});

describe('ACCENT_PRESETS', () => {
  it('has 7 presets', () => {
    expect(ACCENT_PRESETS).toHaveLength(7);
  });

  it('has no duplicate preset ids', () => {
    const ids = ACCENT_PRESETS.map(p => p.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('all presets have required vars', () => {
    for (const preset of ACCENT_PRESETS) {
      expect(preset.vars['--color-accent']).toBeTruthy();
      expect(preset.vars['--color-accent-hover']).toBeTruthy();
      expect(preset.vars['--color-accent-dim']).toBeTruthy();
      expect(preset.vars['--color-accent2']).toBeTruthy();
      expect(preset.vars['--gradient-accent']).toBeTruthy();
    }
  });

  it('default preset is indigo-cyan', () => {
    expect(ACCENT_PRESETS[0].id).toBe('indigo-cyan');
  });
});

describe('applyTheme', () => {
  let root: HTMLElement;

  beforeEach(() => {
    mockMatchMedia(true);
    root = document.createElement('div');
  });

  it('sets data-scheme attribute', () => {
    applyTheme('indigo-cyan', 'dark', root);
    expect(root.getAttribute('data-scheme')).toBe('dark');
  });

  it('sets data-accent attribute', () => {
    applyTheme('hifi-amber', 'dark', root);
    expect(root.getAttribute('data-accent')).toBe('hifi-amber');
  });

  it('sets CSS variables for the chosen accent', () => {
    applyTheme('minimal-teal', 'dark', root);
    expect(root.style.getPropertyValue('--color-accent')).toBe('#14b8a6');
  });

  it('applies dark palette in dark mode', () => {
    applyTheme('indigo-cyan', 'dark', root);
    const bg = root.style.getPropertyValue('--color-bg');
    expect(bg).toBe('#0d0e14');
  });

  it('applies light palette in light mode', () => {
    applyTheme('indigo-cyan', 'light', root);
    const bg = root.style.getPropertyValue('--color-bg');
    expect(bg).toBe('#f1f2f6');
  });

  it('uses system preference when appearance is "system"', () => {
    mockMatchMedia(true); // dark
    applyTheme('indigo-cyan', 'system', root);
    expect(root.getAttribute('data-scheme')).toBe('dark');
  });
});

describe('localStorage persistence', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('stores and reads accent from localStorage', () => {
    localStorage.setItem('ss_accent', 'hifi-amber');
    expect(localStorage.getItem('ss_accent')).toBe('hifi-amber');
  });

  it('stores and reads appearance from localStorage', () => {
    localStorage.setItem('ss_appearance', 'light');
    expect(localStorage.getItem('ss_appearance')).toBe('light');
  });
});
