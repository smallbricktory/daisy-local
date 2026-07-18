// Stateful glue around the pure uiZoom helpers: owns the current zoom,
// applies it to the webview, persists it to settings.ui_zoom, and exposes a
// subscribable store shared by Settings and the keyboard shortcuts.
import { tauri } from '../tauri';
import { applyZoom, clampZoom, installZoomShortcuts, ZOOM_DEFAULT } from './uiZoom';

let current = ZOOM_DEFAULT;
let installed = false;
const subs = new Set<() => void>();
const emit = () => subs.forEach((f) => f());

export function getZoom(): number {
  return current;
}

export function subscribeZoom(cb: () => void): () => void {
  subs.add(cb);
  return () => { subs.delete(cb); };
}

async function persist(z: number): Promise<void> {
  try {
    const s = await tauri.readSettings();
    await tauri.writeSettings({ ...s, ui_zoom: z });
  } catch {
    // Persisting zoom is best-effort; never block the UI on it.
  }
}

/** Set, apply to the webview, notify subscribers, and persist. */
export function setZoom(z: number): void {
  current = clampZoom(z);
  void applyZoom(current);
  emit();
  void persist(current);
}

/**
 * Load the persisted zoom, apply it, and install the Cmd/Ctrl +/-/0 shortcuts.
 * Idempotent — safe to call from app bootstrap effects under StrictMode.
 */
export async function initUiZoom(): Promise<void> {
  if (installed) return;
  installed = true;
  try {
    const s = await tauri.readSettings();
    current = clampZoom(s.ui_zoom ?? ZOOM_DEFAULT);
  } catch {
    current = ZOOM_DEFAULT;
  }
  void applyZoom(current);
  emit();
  installZoomShortcuts(getZoom, (z) => setZoom(z));
}
