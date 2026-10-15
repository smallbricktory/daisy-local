// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Browser-mode world install. `world.ts` is bundled with esbuild and registered
// as a WebDriver BiDi preload script, so its handlers are in
// `window.__wdio_mocks__` before the app's own scripts run and are re-applied on
// every navigation.
import { buildSync } from 'esbuild';
import { resolve } from 'node:path';
import { browser } from '@wdio/globals';
import type { WorldData } from './world.ts';
import { DEV_URL, E2E_PASSPHRASE, isNative } from '../env.ts';
import { unlock } from '../pages/gates.ts';
import { nerdMode } from '../pages/settings.ts';
import { nav } from '../pages/shell.ts';

const WORLD_ENTRY = resolve(import.meta.dirname, 'world.ts');

let worldBundle: string | undefined;
let preloadScriptId: string | undefined;

/** IIFE bundle of `world.ts`, exposing its exports as `__e2eWorld`. Built once
 *  per worker process. */
function bundledWorld(): string {
  if (worldBundle === undefined) {
    const built = buildSync({
      entryPoints: [WORLD_ENTRY],
      bundle: true,
      write: false,
      format: 'iife',
      globalName: '__e2eWorld',
      platform: 'browser',
      target: 'es2022',
    });
    worldBundle = built.outputFiles[0].text;
  }
  return worldBundle;
}

function preloadBody(overrides: Partial<WorldData>): string {
  return `() => {
${bundledWorld()}
const world = __e2eWorld.makeWorld(${JSON.stringify(overrides)});
window.__e2e_world = world;
const internals = (window.__TAURI_INTERNALS__ = window.__TAURI_INTERNALS__ ?? {});
internals.metadata = internals.metadata ?? {
  currentWindow: { label: 'main' },
  currentWebview: { label: 'main', windowLabel: 'main' },
};
// Every reply is cloned at the dispatch boundary, as the native IPC's
// deserialization does. A handler that serves the array or object it keeps
// hands React the reference it already holds, and the setState that follows is
// dropped as unchanged — the screen then sits on stale props over changed data.
// The call itself stays outside the clone so a throw still rejects the invoke.
const replies = {};
for (const [cmd, handler] of Object.entries(world.handlers)) {
  replies[cmd] = async (args) => {
    const value = await handler(args);
    return structuredClone(value);
  };
}
Object.assign(window.__wdio_mocks__ ??= {}, replies);
}`;
}

/** Registers the world as a preload script and reloads the dev server page, so
 *  the app boots against it. Browser mode only. */
export async function installWorld(overrides: Partial<WorldData> = {}): Promise<void> {
  const body = preloadBody({ passphrase: E2E_PASSPHRASE, ...overrides });
  if (preloadScriptId) {
    await browser.scriptRemovePreloadScript({ script: preloadScriptId });
    preloadScriptId = undefined;
  }
  const added = await browser.scriptAddPreloadScript({ functionDeclaration: body });
  preloadScriptId = typeof added === 'string' ? added : added.script;
  await browser.url(DEV_URL);
}

/** Browser mode: install the world. Native mode: nothing — the real backend is
 *  already running. */
export async function ensureWorld(overrides: Partial<WorldData> = {}): Promise<void> {
  if (isNative()) return;
  await installWorld(overrides);
}

/** The standard suite boot: the world in browser mode, the unlocked main shell
 *  in native mode. */
export async function bootWorld(overrides: Partial<WorldData> = {}): Promise<void> {
  await ensureWorld(overrides);
  if (isNative()) {
    await unlock.ensureMain(E2E_PASSPHRASE);
    await nerdMode.set(true);
    await nav.go('library');
  }
}
