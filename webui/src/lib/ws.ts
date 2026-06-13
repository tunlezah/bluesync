import type { WsServerMessage, WsClientMessage } from './types';

export type WsMessageHandler = (msg: WsServerMessage) => void;
export type WsStatusHandler = (connected: boolean) => void;

const RECONNECT_INITIAL_MS = 1_000;
const RECONNECT_MAX_MS = 30_000;
const RECONNECT_FACTOR = 1.5;

export interface WsClient {
  send: (msg: WsClientMessage) => void;
  close: () => void;
  onMessage: (handler: WsMessageHandler) => () => void;
  onStatusChange: (handler: WsStatusHandler) => () => void;
}

export function createWsClient(url: string = '/ws/status'): WsClient {
  let ws: WebSocket | null = null;
  let reconnectDelay = RECONNECT_INITIAL_MS;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;
  let connected = false;

  const messageHandlers = new Set<WsMessageHandler>();
  const statusHandlers = new Set<WsStatusHandler>();

  function notifyStatus(c: boolean) {
    connected = c;
    for (const h of statusHandlers) h(c);
  }

  function notifyMessage(msg: WsServerMessage) {
    for (const h of messageHandlers) h(msg);
  }

  function connect() {
    if (stopped) return;
    // Use absolute ws URL with current origin in browser
    let wsUrl = url;
    if (typeof window !== 'undefined' && !url.startsWith('ws')) {
      const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      wsUrl = `${proto}//${window.location.host}${url}`;
    }
    ws = new WebSocket(wsUrl);

    ws.onopen = () => {
      reconnectDelay = RECONNECT_INITIAL_MS;
      notifyStatus(true);
    };

    ws.onmessage = (evt) => {
      try {
        const msg = JSON.parse(evt.data as string) as WsServerMessage;
        notifyMessage(msg);
      } catch {
        // malformed message — ignore
      }
    };

    ws.onclose = () => {
      ws = null;
      notifyStatus(false);
      scheduleReconnect();
    };

    ws.onerror = () => {
      // onclose will fire after onerror — let it handle cleanup
    };
  }

  function scheduleReconnect() {
    if (stopped) return;
    reconnectTimer = setTimeout(() => {
      reconnectDelay = Math.min(reconnectDelay * RECONNECT_FACTOR, RECONNECT_MAX_MS);
      connect();
    }, reconnectDelay);
  }

  // Start immediately
  connect();

  return {
    send(msg: WsClientMessage) {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify(msg));
      }
    },

    close() {
      stopped = true;
      if (reconnectTimer !== null) {
        clearTimeout(reconnectTimer);
        reconnectTimer = null;
      }
      if (ws) {
        ws.onclose = null; // prevent reconnect loop
        ws.close();
        ws = null;
      }
      notifyStatus(false);
    },

    onMessage(handler: WsMessageHandler) {
      messageHandlers.add(handler);
      return () => messageHandlers.delete(handler);
    },

    onStatusChange(handler: WsStatusHandler) {
      handler(connected); // emit current state immediately
      statusHandlers.add(handler);
      return () => statusHandlers.delete(handler);
    },
  };
}
