// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Injects the selected profile/css theme AFTER the bundled stylesheet as an
// override layer (built-in styles always load; a theme only changes what it
// names). The choice is this machine's css_theme setting.
import { tauri } from '../tauri';

const STYLE_ID = 'profile-theme';

function styleEl(): HTMLStyleElement {
  let el = document.getElementById(STYLE_ID) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement('style');
    el.id = STYLE_ID;
    document.head.appendChild(el);
  }
  return el;
}

/** Applies a theme by stem, or clears the override layer for null. Returns
 *  an error string instead of throwing (a missing/deleted theme file must
 *  never take the UI down). */
export async function applyTheme(name: string | null): Promise<string | null> {
  if (!name) {
    styleEl().textContent = '';
    return null;
  }
  try {
    styleEl().textContent = await tauri.readCssTheme(name);
    return null;
  } catch (e) {
    styleEl().textContent = '';
    return String((e as { message?: unknown })?.message ?? e);
  }
}

/** Startup: apply this machine's stored theme choice, best-effort. */
export async function applyStoredTheme(): Promise<void> {
  try {
    const m = await tauri.machineSettings();
    await applyTheme(m.settings.css_theme ?? null);
  } catch {
    /* built-in look */
  }
}
