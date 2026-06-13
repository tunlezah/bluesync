import type { WsClient } from './ws';
import type { IceCandidate, WsServerMessage } from './types';

export type WebRtcState = 'idle' | 'connecting' | 'playing' | 'error';

export interface WebRtcSession {
  state: WebRtcState;
  error?: string;
  stop: () => void;
}

type StateCallback = (state: WebRtcState, error?: string) => void;

/**
 * Start a WebRTC "recvonly" audio session.
 *
 * - Creates RTCPeerConnection with optional STUN
 * - addTransceiver('audio', {direction:'recvonly'})
 * - createOffer → send webrtc_offer over WS
 * - on webrtc_answer → setRemoteDescription
 * - trickle ICE both ways (exact sdpMid/sdpMLineIndex casing — load-bearing for Safari)
 * - ontrack → plays through an <audio autoplay> element
 * - stop() → webrtc_stop + close
 *
 * Returns a cleanup function.
 */
export function startWebRtcSession(
  ws: WsClient,
  onStateChange: StateCallback,
): () => void {
  let stopped = false;
  let pc: RTCPeerConnection | null = null;
  let audioEl: HTMLAudioElement | null = null;
  let unsubWs: (() => void) | null = null;
  let remoteDescSet = false;
  let pendingCandidates: RTCIceCandidateInit[] = [];

  async function init() {
    try {
      pc = new RTCPeerConnection({
        iceServers: [{ urls: 'stun:stun.l.google.com:19302' }],
      });

      // Attach WS listener for server messages
      unsubWs = ws.onMessage(async (msg: WsServerMessage) => {
        if (stopped || !pc) return;
        try {
          if (msg.type === 'webrtc_answer') {
            await pc.setRemoteDescription({ type: 'answer', sdp: msg.data.sdp });
            remoteDescSet = true;
            for (const c of pendingCandidates) {
              try { await pc.addIceCandidate(new RTCIceCandidate(c)); } catch {}
            }
            pendingCandidates = [];
          } else if (msg.type === 'webrtc_ice_candidate') {
            // sdpMid / sdpMLineIndex are exact camelCase — do NOT rename
            const init: RTCIceCandidateInit = {
              candidate: msg.data.candidate,
              sdpMid: msg.data.sdpMid,
              sdpMLineIndex: msg.data.sdpMLineIndex,
            };
            if (!remoteDescSet) {
              pendingCandidates.push(init);
              return;
            }
            await pc.addIceCandidate(new RTCIceCandidate(init));
          }
        } catch (err) {
          if (!stopped) {
            onStateChange('error', String(err));
          }
        }
      });

      // Trickle ICE: browser → server
      pc.onicecandidate = (evt) => {
        if (stopped || !evt.candidate) return;
        const ice: IceCandidate = {
          candidate: evt.candidate.candidate,
          sdpMid: evt.candidate.sdpMid,       // exact camelCase keys
          sdpMLineIndex: evt.candidate.sdpMLineIndex,
        };
        ws.send({ type: 'webrtc_ice_candidate', data: ice });
      };

      pc.onconnectionstatechange = () => {
        if (stopped || !pc) return;
        const s = pc.connectionState;
        if (s === 'connected') onStateChange('playing');
        else if (s === 'failed' || s === 'disconnected' || s === 'closed') {
          onStateChange('error', `Connection ${s}`);
        }
      };

      // Receive audio track
      pc.ontrack = (evt) => {
        if (stopped) return;
        if (!audioEl) {
          audioEl = document.createElement('audio');
          audioEl.autoplay = true;
          audioEl.setAttribute('playsinline', '');
          document.body.appendChild(audioEl);
        }
        audioEl.srcObject = evt.streams[0] ?? null;
      };

      // recvonly — we don't send audio
      pc.addTransceiver('audio', { direction: 'recvonly' });

      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);

      onStateChange('connecting');
      ws.send({ type: 'webrtc_offer', data: { sdp: offer.sdp! } });
    } catch (err) {
      if (!stopped) {
        onStateChange('error', String(err));
      }
    }
  }

  function cleanup() {
    stopped = true;
    remoteDescSet = false;
    pendingCandidates = [];
    if (unsubWs) { unsubWs(); unsubWs = null; }
    ws.send({ type: 'webrtc_stop' });
    if (pc) { pc.close(); pc = null; }
    if (audioEl) {
      audioEl.srcObject = null;
      audioEl.remove();
      audioEl = null;
    }
    onStateChange('idle');
  }

  init();

  return cleanup;
}
