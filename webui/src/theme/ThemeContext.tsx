import { createContext, ComponentChildren } from 'preact';
import { useContext, useEffect, useState, useCallback } from 'preact/hooks';
import { applyTheme, resolveColorScheme } from './themes';

export type AccentPreset =
  | 'indigo-cyan'
  | 'hifi-amber'
  | 'minimal-teal'
  | 'slate-blue'
  | 'royal-magenta'
  | 'forest-lime'
  | 'desert-red';
export type Appearance = 'light' | 'dark' | 'system';

const LS_ACCENT = 'ss_accent';
const LS_APPEARANCE = 'ss_appearance';

function readAccent(): AccentPreset {
  const v = localStorage.getItem(LS_ACCENT);
  return (
    v === 'indigo-cyan' ||
    v === 'hifi-amber' ||
    v === 'minimal-teal' ||
    v === 'slate-blue' ||
    v === 'royal-magenta' ||
    v === 'forest-lime' ||
    v === 'desert-red'
  ) ? v : 'indigo-cyan';
}

function readAppearance(): Appearance {
  const v = localStorage.getItem(LS_APPEARANCE);
  return (v === 'light' || v === 'dark' || v === 'system') ? v : 'system';
}

export interface ThemeContextValue {
  accent: AccentPreset;
  appearance: Appearance;
  resolvedScheme: 'dark' | 'light';
  setAccent: (a: AccentPreset) => void;
  setAppearance: (a: Appearance) => void;
}

export const ThemeContext = createContext<ThemeContextValue | null>(null);

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error('useTheme must be used inside ThemeProvider');
  return ctx;
}

interface Props { children: ComponentChildren }

export function ThemeProvider({ children }: Props) {
  const [accent, setAccentState] = useState<AccentPreset>(readAccent);
  const [appearance, setAppearanceState] = useState<Appearance>(readAppearance);
  const [resolvedScheme, setResolvedScheme] = useState<'dark' | 'light'>(() => resolveColorScheme(readAppearance()));

  // Apply theme whenever accent or appearance changes
  useEffect(() => {
    applyTheme(accent, appearance);
    setResolvedScheme(resolveColorScheme(appearance));
  }, [accent, appearance]);

  // Listen to system preference changes when appearance === 'system'
  useEffect(() => {
    if (appearance !== 'system') return;
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const handler = () => {
      applyTheme(accent, 'system');
      setResolvedScheme(resolveColorScheme('system'));
    };
    mq.addEventListener('change', handler);
    return () => mq.removeEventListener('change', handler);
  }, [accent, appearance]);

  const setAccent = useCallback((a: AccentPreset) => {
    localStorage.setItem(LS_ACCENT, a);
    setAccentState(a);
  }, []);

  const setAppearance = useCallback((a: Appearance) => {
    localStorage.setItem(LS_APPEARANCE, a);
    setAppearanceState(a);
  }, []);

  const value: ThemeContextValue = { accent, appearance, resolvedScheme, setAccent, setAppearance };
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}
