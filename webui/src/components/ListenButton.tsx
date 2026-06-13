import { useState, useCallback, useEffect, useRef } from 'preact/hooks';
import { startWebRtcSession, type WebRtcState } from '../lib/webrtc';
import type { WsClient } from '../lib/ws';
import { useToast } from './Toast';
import './ListenButton.css';

interface Props {
  ws: WsClient | null;
}

export function ListenButton({ ws }: Props) {
  const [state, setState] = useState<WebRtcState>('idle');
  const [errorMsg, setErrorMsg] = useState<string | undefined>();
  const stopRef = useRef<(() => void) | null>(null);
  const { toast } = useToast();

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (stopRef.current) stopRef.current();
    };
  }, []);

  const handleStart = useCallback(() => {
    if (!ws) {
      toast('Not connected to server', 'error');
      return;
    }
    if (state !== 'idle' && state !== 'error') return;

    const stop = startWebRtcSession(ws, (s, err) => {
      setState(s);
      setErrorMsg(err);
      if (s === 'error') {
        toast(`Listen error: ${err ?? 'unknown'}`, 'error');
      }
    });
    stopRef.current = stop;
  }, [ws, state, toast]);

  const handleStop = useCallback(() => {
    if (stopRef.current) {
      stopRef.current();
      stopRef.current = null;
    }
    setState('idle');
    setErrorMsg(undefined);
  }, []);

  const isActive = state === 'connecting' || state === 'playing';

  return (
    <div class="listen-btn-wrap">
      {isActive ? (
        <button class="btn listen-btn listen-btn--active" onClick={handleStop} aria-label="Stop listening in browser">
          <span class={`listen-btn__dot status-dot ${state === 'connecting' ? 'status-dot--connecting' : 'status-dot--active'}`} />
          {state === 'connecting' ? 'Connecting…' : '▮ Stop listening'}
        </button>
      ) : (
        <button
          class="btn btn-ghost listen-btn"
          onClick={handleStart}
          disabled={!ws}
          aria-label="Listen in this browser via WebRTC"
        >
          <span class="listen-btn__icon" aria-hidden="true">▶</span>
          Listen in this browser
        </button>
      )}
      {state === 'error' && errorMsg && (
        <p class="listen-btn__error" role="alert">{errorMsg}</p>
      )}
    </div>
  );
}
