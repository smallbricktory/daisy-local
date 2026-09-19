// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Light / dark / follow-the-OS, applied as `data-theme` on <html>.
//
// The stylesheet defines every colour as a semantic role and flips those
// roles under `:root[data-theme="dark"]`, so switching a theme is one
// attribute write — no per-component work, no second stylesheet.
//
// "System" asks the WINDOW for the OS theme rather than trusting the CSS
// media query: `prefers-color-scheme` is reliable on WKWebView and WebView2
// but depends on the GTK theme on WebKitGTK, where it can report light on a
// dark desktop. The media query stays as the fallback (and is what answers
// in a plain browser, e.g. the dev stub).
import { getCurrentWindow } from '@tauri-apps/api/window';
import { emit, listen } from '@tauri-apps/api/event';
import { tauri } from '../tauri';

// The mini and reminder popups are separate webviews, pre-rendered hidden at
// startup, and they read the stored choice once. A change made in Settings
// reaches them only as an event.
const APPEARANCE_EVENT = 'appearance:changed';

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

/** Apply a choice in this webview and every other one. */
export async function setAppearance(choice: Appearance): Promise<void> {
  await applyAppearance(choice);
  void emit(APPEARANCE_EVENT, choice).catch(() => { /* no window (dev/browser) */ });
}

/** The app's answer on translucency, written where the stylesheet reads it.
 *
 *  WebKit answers no media query for macOS Reduce Transparency, and WebKitGTK
 *  blurs on its accelerated path alone, so the app asks the Rust side.
 *  Chromium (Windows) answers `prefers-reduced-transparency` itself, and the
 *  stylesheet takes either answer. */
async function paintTransparency(): Promise<void> {
  try {
    const reduce = await tauri.reduceTransparency();
    if (reduce) document.documentElement.dataset.reduceTransparency = '1';
    else delete document.documentElement.dataset.reduceTransparency;
  } catch {
    /* not in a Tauri window (dev/browser) — the media query answers */
  }
}

/** Startup: paint this machine's stored choice, best-effort, and follow
 *  changes made in whichever webview owns Settings. */
export async function initAppearance(): Promise<void> {
  void listen<Appearance>(APPEARANCE_EVENT, ({ payload }) => { void applyAppearance(payload); })
    .catch(() => { /* no window (dev/browser) */ });
  // Two independent reads of this machine's state, asked together: the stored
  // theme and the OS transparency setting.
  await Promise.all([
    tauri.machineSettings()
      .then((m) => applyAppearance((m.settings.appearance as Appearance) ?? 'system'))
      .catch(() => applyAppearance('system')),
    paintTransparency(),
  ]);
  // The setting is changed in System Settings, which takes the focus to do it;
  // coming back is when a stale answer shows.
  void getCurrentWindow()
    .onFocusChanged(({ payload: focused }) => { if (focused) void paintTransparency(); })
    .catch(() => { /* no window (dev/browser) */ });
}
