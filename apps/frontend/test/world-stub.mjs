// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The e2e fixture world, installed into a Playwright page.
//
// `e2e/fixtures/world.ts` is the one stubbed backend in this repo. The wdio
// suites reach it through `e2e/fixtures/install.ts`; the design-system
// harnesses — `contrast.mjs`, `capture-review.mjs` — reach it through here.
// Both bundle the same module.
//
// Every unanswered command lands in `window.__DAISY_STUB_MISSES`; read it with
// `stubMisses(page)`.
import { buildSync } from 'esbuild';
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';

const WORLD_ENTRY = resolve(import.meta.dirname, '../e2e/fixtures/world.ts');

let bundle;

/** IIFE bundle of `world.ts`, exposing its exports as `__e2eWorld`. Built once
 *  per process. */
function bundledWorld() {
  if (bundle === undefined) {
    const built = buildSync({
      entryPoints: [WORLD_ENTRY],
      bundle: true,
      write: false,
      format: 'iife',
      globalName: '__e2eWorld',
      platform: 'browser',
      target: 'es2022',
    });
    bundle = built.outputFiles[0].text;
  }
  return bundle;
}

/** Registers the world as an init script, so its handlers answer from the
 *  page's first script onward and across navigations. Call before the first
 *  `page.goto`.
 *
 *  `label` is the webview label `main.tsx` branches on: 'mini' and 'reminder'
 *  render those popups in place of <App/>. */
export async function installWorld(page, overrides = {}, label = 'main') {
  await page.addInitScript(`(() => {
${bundledWorld()}
const world = __e2eWorld.makeWorld(${JSON.stringify(overrides)});
const label = ${JSON.stringify(label)};
window.__e2e_world = world;
window.__DAISY_STUB_MISSES = [];
const internals = (window.__TAURI_INTERNALS__ = window.__TAURI_INTERNALS__ ?? {});
internals.metadata = {
  currentWindow: { label },
  currentWebview: { label, windowLabel: label },
  windows: [{ label }],
  webviews: [{ label, windowLabel: label }],
};
internals.plugins = {};
internals.convertFileSrc = (path) => path;
internals.transformCallback = (cb) => {
  const id = Math.floor(Math.random() * 1e9);
  window['_' + id] = cb;
  return id;
};
internals.invoke = (cmd, args) => {
  // Event listen/unlisten and the window plugins resolve with null.
  if (cmd.startsWith('plugin:')) return Promise.resolve(null);
  const handler = world.handlers[cmd];
  if (!handler) {
    window.__DAISY_STUB_MISSES.push(cmd);
    return Promise.reject(new Error('stub: no backend for ' + cmd));
  }
  try {
    // Replies are cloned at the dispatch boundary.
    return Promise.resolve(handler(args ?? null)).then((v) => structuredClone(v));
  } catch (e) {
    return Promise.reject(e);
  }
};
})()`);
}

/** Walk the app's own surfaces in `page`, calling `at(kind, name)` on each one
 *  once it has settled: every routed rail item ('route'), every Settings
 *  section ('settings'), a session as it opens ('session') and each of its
 *  detail tabs ('detail'). The lists are read from the DOM.
 *
 *  Mini and Help are skipped. */
export async function walkSurfaces(page, at, settle = 450) {
  const railKeys = await page.evaluate(() => [...document.querySelectorAll('.rail .nav-item')]
    .map((b) => b.dataset.testid)
    .filter((t) => t && t.startsWith('nav-') && !['nav-mini', 'nav-settings', 'nav-help'].includes(t))
    .map((t) => t.slice(4)));
  for (const key of railKeys) {
    await page.locator(`[data-testid="nav-${key}"]`).first().click();
    await page.waitForTimeout(settle);
    await at('route', key);
  }

  await page.locator('[data-testid="nav-settings"]').first().click();
  await page.waitForSelector('.settings-tab', { timeout: 10_000 });
  const sections = await page.evaluate(() => [...document.querySelectorAll('.settings-tab')]
    .map((b) => b.textContent.trim()));
  for (const section of sections) {
    await page.locator('.settings-tab', { hasText: section }).first().click();
    await page.waitForTimeout(settle);
    await at('settings', section);
  }

  await page.locator('[data-testid="nav-library"]').first().click();
  await page.waitForTimeout(400);
  await page.getByText('Product sync — Q3 roadmap').first().click();
  await page.waitForSelector('.detail-tabs__tab', { timeout: 10_000 });
  await at('session', 'detail');
  const tabs = await page.evaluate(() => [...document.querySelectorAll('.detail-tabs__tab')]
    .map((b) => b.textContent.trim()));
  for (const tab of tabs) {
    await page.locator('.detail-tabs__tab', { hasText: tab }).first().click();
    await page.waitForTimeout(400);
    await at('detail', tab);
  }
}

/** Walks a page booted at the first-run wizard to its speaker-names step:
 *  Get started → a name → the mic picker → speaker names. */
export async function walkWizardToNames(page, settle = 450) {
  await page.locator('[data-testid="wizard-get-started"]').first().click();
  await page.waitForSelector('[data-testid="wizard-intro-name"]', { timeout: 10_000 });
  await page.locator('[data-testid="wizard-intro-name"]').first().fill('Alex');
  await page.locator('[data-testid="wizard-intro-continue"]').first().click();
  await page.waitForSelector('[data-testid="wizard-mic-continue"]', { timeout: 10_000 });
  await page.locator('[data-testid="wizard-mic-continue"]').first().click();
  await page.waitForSelector('[data-testid="wizard-names-continue"]', { timeout: 10_000 });
  await page.waitForTimeout(settle);
}

/** Continues from the speaker-names step to the closing plan page. */
export async function walkWizardToPlan(page, settle = 450) {
  await walkWizardToNames(page, settle);
  await page.locator('[data-testid="wizard-names-continue"]').first().click();
  await page.waitForSelector('[data-testid="plan-row-ai"]', { timeout: 10_000 });
  await page.waitForTimeout(settle);
}

/** The distinct commands the world could not answer, in first-seen order. */
export async function stubMisses(page) {
  return page.evaluate(() => [...new Set(window.__DAISY_STUB_MISSES ?? [])]);
}

/** The stub dev server, started if nothing is already serving `port`. Returns
 *  a stop function (a no-op when the server was already up). */
export async function ensureDevServer(port) {
  const url = `http://127.0.0.1:${port}/`;
  const up = async () => {
    try { return (await fetch(url, { signal: AbortSignal.timeout(1000) })).ok; } catch { return false; }
  };
  if (await up()) return () => {};

  const vite = spawn(process.execPath, ['node_modules/vite/bin/vite.js', '--port', String(port), '--strictPort'], {
    cwd: resolve(import.meta.dirname, '..'),
    stdio: 'ignore',
  });
  for (let i = 0; i < 60; i++) {
    if (await up()) return () => vite.kill();
    await new Promise((r) => setTimeout(r, 500));
  }
  vite.kill();
  throw new Error(`the stub dev server did not come up on ${port}`);
}
