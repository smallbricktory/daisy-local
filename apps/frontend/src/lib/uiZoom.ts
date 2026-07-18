// Whole-UI zoom via the native webview zoom factor (Tauri v2). Scales all text
// and spacing like a browser's Cmd+/Cmd-, persisted as settings.ui_zoom.
import { getCurrentWebview } from '@tauri-apps/api/webview';

export const ZOOM_MIN = 0.5;
export const ZOOM_MAX = 2.0;
export const ZOOM_STEP = 0.1;
export const ZOOM_DEFAULT = 1.0;

/** Clamp to [MIN,MAX] and round to one decimal (kills float drift like 1.0000001). */
export function clampZoom(n: number): number {
  if (typeof n !== 'number' || !Number.isFinite(n)) return ZOOM_DEFAULT;
  const rounded = Math.round(n * 10) / 10;
  return Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, rounded));
}

export type ZoomDir = 'in' | 'out' | 'reset';

export function stepZoom(current: number, dir: ZoomDir): number {
  if (dir === 'reset') return ZOOM_DEFAULT;
  const base = clampZoom(current);
  return clampZoom(base + (dir === 'in' ? ZOOM_STEP : -ZOOM_STEP));
}

/** Pure key→action mapping; no DOM access. */
export function zoomActionForKey(e: {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  isMac: boolean;
}): ZoomDir | null {
  const mod = e.isMac ? e.metaKey : e.ctrlKey;
  if (!mod) return null;
  if (e.key === '=' || e.key === '+') return 'in';
  if (e.key === '-' || e.key === '_') return 'out';
  if (e.key === '0') return 'reset';
  return null;
}

export function isMacPlatform(): boolean {
  if (typeof navigator === 'undefined') return false;
  return /mac/i.test(navigator.platform || navigator.userAgent || '');
}

/** Apply the zoom factor to the webview. Best-effort, but surfaces failures to
 *  the console — a silently-swallowed error here once hid a missing
 *  `core:webview:allow-set-webview-zoom` capability (zoom did nothing). */
export async function applyZoom(factor: number): Promise<void> {
  try {
    await getCurrentWebview().setZoom(clampZoom(factor));
  } catch (e) {
    console.warn('applyZoom: setZoom failed (check webview zoom capability):', e);
  }
}

/**
 * Install the global Cmd/Ctrl +/-/0 shortcut handler. `get` returns the current
 * zoom, `set` persists+applies the new one. Returns an unsubscribe fn.
 */
export function installZoomShortcuts(
  get: () => number,
  set: (z: number) => void,
): () => void {
  const isMac = isMacPlatform();
  const onKey = (ev: KeyboardEvent) => {
    const action = zoomActionForKey({
      key: ev.key,
      metaKey: ev.metaKey,
      ctrlKey: ev.ctrlKey,
      isMac,
    });
    if (!action) return;
    ev.preventDefault();
    set(stepZoom(get(), action));
  };
  window.addEventListener('keydown', onKey);
  return () => window.removeEventListener('keydown', onKey);
}
