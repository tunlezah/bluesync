import type { AppStateSnapshot, EqInfo, OutputKind, OutputState } from './types';

export type ApiResult<T> =
  | { ok: true; data: T; status: number }
  | { ok: false; status: number; message: string };

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<ApiResult<T>> {
  try {
    const init: RequestInit = {
      method,
      headers: body !== undefined ? { 'Content-Type': 'application/json' } : {},
    };
    if (body !== undefined) {
      init.body = JSON.stringify(body);
    }
    const res = await fetch(path, init);
    if (res.ok || res.status === 202) {
      // Try to parse JSON; some 202s have no body
      let data: T | undefined;
      const text = await res.text();
      if (text) {
        try { data = JSON.parse(text) as T; } catch { /* no body */ }
      }
      return { ok: true, data: data as T, status: res.status };
    }
    const errText = await res.text().catch(() => '');
    return { ok: false, status: res.status, message: errText || res.statusText };
  } catch (err) {
    return { ok: false, status: 0, message: String(err) };
  }
}

// ── Status ─────────────────────────────────────────────────────────────────

export function getStatus(): Promise<ApiResult<AppStateSnapshot>> {
  return request<AppStateSnapshot>('GET', '/api/status');
}

// ── Pairing ────────────────────────────────────────────────────────────────

export function startPairing(seconds?: number): Promise<ApiResult<void>> {
  const url = seconds !== undefined ? `/api/pair?seconds=${seconds}` : '/api/pair';
  return request<void>('POST', url);
}

export function stopPairing(): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/pair/close');
}

// ── Media ──────────────────────────────────────────────────────────────────

export type MediaAction = 'play' | 'pause' | 'next' | 'previous' | 'stop';

export function postMedia(action: MediaAction): Promise<ApiResult<void>> {
  return request<void>('POST', `/api/media/${action}`);
}

// ── EQ ─────────────────────────────────────────────────────────────────────

export function getEq(): Promise<ApiResult<EqInfo>> {
  return request<EqInfo>('GET', '/api/eq');
}

export interface EqUpdate {
  enabled?: boolean;
  gains?: number[];
}

export function postEq(update: EqUpdate): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/eq', update);
}

// ── Settings ───────────────────────────────────────────────────────────────

export function postSettingsName(name: string): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/settings/name', { name });
}

// ── Bluetooth device commands ──────────────────────────────────────────────

export type DeviceCmd = 'connect' | 'disconnect' | 'forget';

export function postDeviceCmd(addr: string, cmd: DeviceCmd): Promise<ApiResult<void>> {
  return request<void>('POST', `/api/bluetooth/${encodeURIComponent(addr)}/${cmd}`);
}

// ── Admin ──────────────────────────────────────────────────────────────────

export function postAdminRestart(): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/admin/restart');
}

export function postAdminIdentify(): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/admin/identify');
}

// ── Output ─────────────────────────────────────────────────────────────────

/** GET /api/outputs — optional; the WS snapshot already carries output state */
export function getOutputs(): Promise<ApiResult<OutputState>> {
  return request<OutputState>('GET', '/api/outputs');
}

/** POST /api/output { type, id } → 202 ok / 400 bad / 503 unavailable */
export function setOutput(type: OutputKind, id: string): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/output', { type, id });
}

/** POST /api/output { type: "none", id: "" } — tear down server output */
export function setOutputNone(): Promise<ApiResult<void>> {
  return request<void>('POST', '/api/output', { type: 'none', id: '' });
}

// ── Helpers ────────────────────────────────────────────────────────────────

/** Returns true if the failure was a 404 (endpoint not built yet) */
export function isNotFound(result: ApiResult<unknown>): boolean {
  return !result.ok && result.status === 404;
}

/** Returns true if the failure looks like a network error or 503 */
export function isUnavailable(result: ApiResult<unknown>): boolean {
  return !result.ok && (result.status === 503 || result.status === 0);
}
