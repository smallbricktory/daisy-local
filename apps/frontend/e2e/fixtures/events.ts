// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Fires a backend event into the page in either mode.
//
// Browser mode goes through the service's mock event registry. Native mode
// emits through `window.__TAURI__.event` — the ui-test build sets
// `withGlobalTauri`, and the service's own `emitEvent` reaches for a Tauri
// event API the app doesn't expose (it imports @tauri-apps/api as a module).
import { browser } from '@wdio/globals';
import { isNative } from '../env.ts';

type TauriGlobal = {
  __TAURI__?: { event?: { emit: (name: string, payload: unknown) => Promise<void> } };
};

export async function emitAppEvent(name: string, payload: unknown): Promise<void> {
  if (!isNative()) {
    await browser.tauri.emitEvent(name, payload);
    return;
  }
  await browser.execute(async (n: string, p: unknown) => {
    const api = (window as unknown as TauriGlobal).__TAURI__?.event;
    if (!api) throw new Error('window.__TAURI__.event missing — is this a ui-test build?');
    await api.emit(n, p);
  }, name, payload);
}

/** Stands in for the `library:changed` a mutation command emits. The browser
 *  world holds state and fires no events, and that event is what invalidates
 *  the suggestion and session caches. */
export async function emitLibraryChanged(sessionId: string): Promise<void> {
  await emitAppEvent('library:changed', { kind: 'updated', session_id: sessionId });
}
