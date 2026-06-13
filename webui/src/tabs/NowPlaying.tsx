import { useRef, useEffect, useCallback } from 'preact/hooks';
import { useStore } from '../lib/store';
import { postMedia } from '../lib/api';
import { useToast } from '../components/Toast';
import { ListenButton } from '../components/ListenButton';
import { OutputPicker } from '../components/OutputPicker';
import type { MediaInfo } from '../lib/types';
import './NowPlaying.css';

// ── Spectrum canvas ────────────────────────────────────────────────────────

interface SpectrumProps {
  bands: number[];
}

function SpectrumCanvas({ bands }: SpectrumProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const rafRef = useRef<number>(0);
  const bandsRef = useRef(bands);

  useEffect(() => {
    bandsRef.current = bands;
  }, [bands]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    function draw() {
      const c = canvasRef.current;
      if (!c || !ctx) return;
      // Idle cheaply when the canvas is not visible
      if (!c.offsetWidth) {
        rafRef.current = requestAnimationFrame(draw);
        return;
      }
      const dpr = window.devicePixelRatio || 1;
      const W = c.offsetWidth;
      const H = c.offsetHeight;
      if (c.width !== W * dpr || c.height !== H * dpr) {
        c.width = W * dpr;
        c.height = H * dpr;
      }
      // Set transform every frame — deterministic regardless of resize history
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, W, H);

      const b = bandsRef.current;
      if (!b.length) {
        rafRef.current = requestAnimationFrame(draw);
        return;
      }

      // Get accent colour from CSS variable
      const root = document.documentElement;
      const accentColor = root.style.getPropertyValue('--color-accent').trim() || '#6366f1';
      const accent2 = root.style.getPropertyValue('--color-accent2').trim() || '#06b6d4';

      const barCount = b.length;
      const gap = 2;
      const barW = Math.max(1, (W - gap * (barCount - 1)) / barCount);

      const grad = ctx.createLinearGradient(0, H, W, 0);
      grad.addColorStop(0, accentColor);
      grad.addColorStop(1, accent2);

      ctx.fillStyle = grad;

      for (let i = 0; i < barCount; i++) {
        const v = Math.min(1, Math.max(0, b[i]));
        const barH = Math.max(2, v * (H - 4));
        const x = i * (barW + gap);
        const y = H - barH;
        ctx.beginPath();
        const r = Math.min(2, barW / 2);
        ctx.roundRect(x, y, barW, barH, [r, r, 0, 0]);
        ctx.fill();
      }

      rafRef.current = requestAnimationFrame(draw);
    }

    rafRef.current = requestAnimationFrame(draw);
    return () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
    };
  }, []); // only run once; bands fed via ref

  return (
    <canvas
      ref={canvasRef}
      class="spectrum-canvas"
      aria-label="Audio spectrum visualizer"
      aria-hidden="true"
    />
  );
}

// ── Transport controls ────────────────────────────────────────────────────

interface TransportProps {
  media: MediaInfo | null;
  onAction: (action: string) => void;
}

function TransportControls({ media, onAction }: TransportProps) {
  const isPlaying = media?.status === 'playing';
  const hasMedia = media !== null;

  return (
    <div class="transport" role="group" aria-label="Playback controls">
      <button
        class="transport__btn transport__btn--secondary"
        onClick={() => onAction('previous')}
        disabled={!hasMedia}
        aria-label="Previous track"
        title="Previous"
      >
        ⏮
      </button>
      <button
        class="transport__btn transport__btn--primary"
        onClick={() => onAction(isPlaying ? 'pause' : 'play')}
        disabled={!hasMedia}
        aria-label={isPlaying ? 'Pause' : 'Play'}
        title={isPlaying ? 'Pause' : 'Play'}
      >
        {isPlaying ? '⏸' : '▶'}
      </button>
      <button
        class="transport__btn transport__btn--secondary"
        onClick={() => onAction('next')}
        disabled={!hasMedia}
        aria-label="Next track"
        title="Next"
      >
        ⏭
      </button>
      <button
        class="transport__btn transport__btn--stop"
        onClick={() => onAction('stop')}
        disabled={!hasMedia}
        aria-label="Stop"
        title="Stop"
      >
        ⏹
      </button>
    </div>
  );
}

// ── Album art ─────────────────────────────────────────────────────────────

interface AlbumArtProps {
  url: string | null;
}

function AlbumArt({ url }: AlbumArtProps) {
  // When the image 404s or fails for any reason, fall back to the gradient.
  // We use a hidden <img> that swaps in over the gradient when it loads.
  return (
    <div class="album-art" aria-hidden="true">
      <div class="album-art__gradient" />
      <div class="album-art__note">♪</div>
      {url && (
        <img
          src={url}
          alt="album art"
          class="album-art__img"
          onError={(e) => {
            (e.currentTarget as HTMLImageElement).style.display = 'none';
          }}
        />
      )}
    </div>
  );
}

// ── Main tab ──────────────────────────────────────────────────────────────

export function NowPlayingTab() {
  const store = useStore();
  const { toast } = useToast();
  const { media, spectrum } = store;

  const handleAction = useCallback(async (action: string) => {
    const result = await postMedia(action as 'play' | 'pause' | 'next' | 'previous' | 'stop');
    if (!result.ok) {
      if (result.status === 503) toast('Playback not available', 'warning');
      else if (result.status !== 202) toast(`Media error: ${result.message}`, 'error');
    }
  }, [toast]);

  const title = media?.title ?? null;
  const artist = media?.artist ?? null;
  const album = media?.album ?? null;
  const isPlaying = media?.status === 'playing';
  const hasMedia = media !== null;

  return (
    <div class="tab-content now-playing">
      {/* Album art + metadata */}
      <div class="card now-playing__hero">
        <AlbumArt url={media?.artwork_url ?? null} />
        <div class="now-playing__meta">
          {hasMedia ? (
            <>
              <p class="now-playing__title">{title ?? 'Unknown Title'}</p>
              <p class="now-playing__artist">{artist ?? 'Unknown Artist'}</p>
              {album && <p class="now-playing__album">{album}</p>}
              <span class={`now-playing__status-badge ${isPlaying ? 'now-playing__status-badge--playing' : ''}`}>
                {isPlaying ? '▸ Playing' : media?.status === 'paused' ? '⏸ Paused' : '⏹ Stopped'}
              </span>
            </>
          ) : (
            <>
              <p class="now-playing__title now-playing__title--idle">Nothing playing</p>
              <p class="now-playing__artist">Connect a Bluetooth device and play something</p>
            </>
          )}
        </div>
      </div>

      {/* Spectrum visualizer */}
      <div class="card now-playing__spectrum-card">
        <SpectrumCanvas bands={spectrum} />
      </div>

      {/* Transport */}
      <div class="card now-playing__controls">
        <TransportControls media={media} onAction={handleAction} />
      </div>

      {/* Listen in browser (WebRTC) */}
      <div class="card now-playing__listen">
        <ListenButton ws={store.ws} />
      </div>

      {/* Output picker — choose where server audio plays */}
      <div class="card now-playing__output">
        <OutputPicker />
      </div>
    </div>
  );
}
