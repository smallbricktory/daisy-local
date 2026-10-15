// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Stateful glue around the pure uiZoom helpers: the zoom is this machine's
// ui_zoom in the machine store; this module applies it to the webview as it
// changes and writes it from Settings and the keyboard shortcuts.
import { machineStore, setMachine, useMachineSettings } from './settingsStore';
import { applyZoom, clampZoom, installZoomShortcuts, ZOOM_DEFAULT } from './uiZoom';

let installed = false;

export function getZoom(): number {
  return clampZoom(machineStore.get()?.settings.ui_zoom ?? ZOOM_DEFAULT);
}

/** React hook: the zoom this machine stores. */
export function useZoom(): number {
  return clampZoom(useMachineSettings()?.ui_zoom ?? ZOOM_DEFAULT);
}

/** Stores a zoom; the webview follows the store. */
export function setZoom(z: number): void {
  // Best-effort persist.
  void setMachine({ ui_zoom: clampZoom(z) }).catch(() => {});
}

/**
 * Applies the stored zoom now and on every change, and installs the
 * Cmd/Ctrl +/-/0 shortcuts. Idempotent — safe to call from app bootstrap
 * effects under StrictMode.
 */
export function initUiZoom(): void {
  if (installed) return;
  installed = true;
  let applied: number | null = null;
  const follow = () => {
    if (machineStore.get() === null) return;
    const z = getZoom();
    if (z === applied) return;
    applied = z;
    void applyZoom(z);
  };
  machineStore.subscribe(follow);
  follow();
  installZoomShortcuts(getZoom, setZoom);
}
