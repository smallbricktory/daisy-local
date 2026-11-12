// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Light / dark / follow-the-OS, applied as `data-theme` on <html>.
//
// The stylesheet defines every colour as a semantic role and flips those
// roles under `:root[data-theme="dark"]`.
//
// "System" asks the window for the OS theme and falls back to the
// `prefers-color-scheme` media query.
import { getCurrentWindow } from '@tauri-apps/api/window';
import { tauri } from '../tauri';
import { machineStore, setMachine } from './settingsStore';

export type Appearance = 'light' | 'dark' | 'system';

const media = () => window.matchMedia('(prefers-color-scheme: dark)');

/** The OS's current preference. Window theme first, media query second. */
async function systemPrefersDark(): Promise<boolean> {
  try {
    const t = await getCurrentWindow().theme();
    if (t === 'dark' || t === 'light') return t === 'dark';
  } catch {
    /* not in a Tauri window (dev/browser) — fall through */
  }
  return media().matches;
}

function paint(dark: boolean): void {
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}

/** Sets the window's own theme, which drives OS-drawn chrome such as the
 *  title bar. `null` on 'system' leaves the window following the OS. */
function paintWindow(choice: Appearance, dark: boolean): void {
  void getCurrentWindow()
    .setTheme(choice === 'system' ? null : dark ? 'dark' : 'light')
    .catch(() => { /* not in a Tauri window, or the platform has no say */ });
}

let current: Appearance = 'system';
let watching = false;

/** Start following OS changes, once per process. Only repaints on 'system'. */
function watchSystem(): void {
  if (watching) return;
  watching = true;
  media().addEventListener('change', (e) => {
    if (current === 'system') paint(e.matches);
  });
  // The window event is authoritative where the media query is not.
  void getCurrentWindow()
    .onThemeChanged(({ payload }) => {
      if (current === 'system') paint(payload === 'dark');
    })
    .catch(() => { /* no window (dev/browser): the media listener covers it */ });
}

/** Apply a choice now, and keep following the OS while it is 'system'. */
export async function applyAppearance(choice: Appearance): Promise<void> {
  current = choice;
  const dark = choice === 'dark' || (choice === 'system' && (await systemPrefersDark()));
  paint(dark);
  paintWindow(choice, dark);
  watchSystem();
}

/** Stores a choice; every webview follows the machine store. */
export function setAppearance(choice: Appearance): Promise<void> {
  return setMachine({ appearance: choice });
}

/** The app's answer on translucency, written where the stylesheet reads it. */
async function paintTransparency(): Promise<void> {
  try {
    const reduce = await tauri.reduceTransparency();
    if (reduce) document.documentElement.dataset.reduceTransparency = '1';
    else delete document.documentElement.dataset.reduceTransparency;
  } catch {
    /* not in a Tauri window (dev/browser) — the media query answers */
  }
}

/** Startup: paint this machine's stored choice, and follow every change to
 *  it, whichever webview or command made it. With no answer from the backend
 *  the app follows the OS. */
export async function initAppearance(): Promise<void> {
  let painted: Appearance | null = null;
  const follow = () => {
    const choice = (machineStore.get()?.settings.appearance as Appearance | undefined) ?? 'system';
    if (choice === painted) return;
    painted = choice;
    void applyAppearance(choice);
  };
  machineStore.subscribe(follow);
  // Two independent reads of this machine's state, asked together: the stored
  // theme and the OS transparency setting.
  await Promise.all([machineStore.refresh().catch(() => {}).then(follow), paintTransparency()]);
  void getCurrentWindow()
    .onFocusChanged(({ payload: focused }) => { if (focused) void paintTransparency(); })
    .catch(() => { /* no window (dev/browser) */ });
}
