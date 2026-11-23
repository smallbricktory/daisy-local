// Draw numbered callouts over review shots from a candidates file.
//
// Drives the e2e fixture world like capture-review.mjs, but at every surface
// it outlines the elements a candidates file names for that surface, numbers
// them, and writes one annotated PNG per surface plus `boxes.json` (the CSS
// pixel box of every callout) for a page that wants hotspots.
//
// Usage, from apps/frontend:
//   node test/annotate-candidates.mjs <candidates.json> <out-dir>
//
// candidates.json: [{ id, surface, selector, ... }] — `surface` is the shot
// basename without theme (route-library, settings-recordings, detail-summary,
// session-detail, live-recording, window-mini, window-mini-recording,
// window-reminder). A selector that resolves to nothing is reported in
// `boxes.json` under `missing` instead of failing the run.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { webkit } from 'playwright';
import { ensureDevServer, installWorld, walkSurfaces } from './world-stub.mjs';

const [CANDIDATES, OUT] = process.argv.slice(2);
if (!CANDIDATES || !OUT) throw new Error('usage: annotate-candidates.mjs <candidates.json> <out-dir>');
mkdirSync(OUT, { recursive: true });

const candidates = JSON.parse(readFileSync(CANDIDATES, 'utf8')).filter((c) => c.selector);
const bySurface = new Map();
for (const c of candidates) {
  if (!bySurface.has(c.surface)) bySurface.set(c.surface, []);
  bySurface.get(c.surface).push(c);
}

const PORT = 5199;
const APP = `http://127.0.0.1:${PORT}/`;
const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

const boxes = {};
const missing = [];

/** Outlines every candidate of `surface` in the page, screenshots, cleans up. */
async function annotate(page, surface) {
  const list = bySurface.get(surface) ?? [];
  const result = await page.evaluate((items) => {
    const found = [];
    const lost = [];
    for (const { id, selector } of items) {
      let el = null;
      try { el = document.querySelector(selector); } catch { el = null; }
      if (!el) { lost.push(id); continue; }
      const r = el.getBoundingClientRect();
      found.push({ id, x: r.x, y: r.y, w: r.width, h: r.height });
      const box = document.createElement('div');
      box.className = '__callout';
      box.style.cssText = `position:fixed;left:${r.x - 3}px;top:${r.y - 3}px;width:${r.width + 6}px;height:${r.height + 6}px;`
        + 'outline:2px solid #d0342c;outline-offset:0;border-radius:4px;pointer-events:none;z-index:2147483646;box-sizing:border-box;';
      const badge = document.createElement('div');
      badge.className = '__callout';
      badge.textContent = String(id);
      badge.style.cssText = `position:fixed;left:${Math.max(0, r.x - 14)}px;top:${Math.max(0, r.y - 14)}px;`
        + 'min-width:22px;height:22px;padding:0 6px;border-radius:11px;background:#d0342c;color:#fff;'
        + 'font:700 13px/22px system-ui,sans-serif;text-align:center;pointer-events:none;z-index:2147483647;box-shadow:0 1px 3px rgba(0,0,0,.4);';
      document.body.append(box, badge);
    }
    return { found, lost };
  }, list.map(({ id, selector }) => ({ id, selector })));
  boxes[surface] = result.found;
  for (const id of result.lost) missing.push({ id, surface });
  await page.waitForTimeout(150);
  await page.screenshot({ path: `${OUT}/${surface}.png` });
  await page.evaluate(() => { for (const el of document.querySelectorAll('.__callout')) el.remove(); });
  console.log(`${surface}: ${result.found.length} callouts${result.lost.length ? `, ${result.lost.length} missing` : ''}`);
}

const stopServer = await ensureDevServer(PORT);
const browser = await webkit.launch();
const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, deviceScaleFactor: 2 });
const page = await context.newPage();
await installWorld(page);
await page.goto(APP, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.rail .nav-item', { timeout: 15_000 });
await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });

const NAME = {
  route: (n) => `route-${n}`,
  settings: (n) => `settings-${slug(n)}`,
  session: () => 'session-detail',
  detail: (n) => `detail-${slug(n)}`,
};
await walkSurfaces(page, async (kind, name) => {
  await page.waitForTimeout(350);
  await annotate(page, NAME[kind](name));
}, 500);

const SATELLITES = [
  { name: 'window-mini', label: 'mini', size: { width: 360, height: 260 }, overrides: {} },
  {
    name: 'window-mini-recording', label: 'mini', size: { width: 360, height: 260 },
    overrides: { recording: { state: 'recording', session_id: 's-live', started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754 } },
  },
  { name: 'window-reminder', label: 'reminder', size: { width: 320, height: 150 }, overrides: { reminderTitle: 'Product sync — Q3 roadmap' } },
];
for (const sat of SATELLITES) {
  const p = await context.newPage();
  await p.setViewportSize(sat.size);
  await installWorld(p, sat.overrides, sat.label);
  await p.goto(APP, { waitUntil: 'domcontentloaded' });
  await p.waitForSelector('.mini, .reminder', { timeout: 15_000 });
  await p.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
  await p.waitForTimeout(350);
  await annotate(p, sat.name);
  await p.close();
}

const live = await context.newPage();
await live.setViewportSize({ width: 1280, height: 900 });
await installWorld(live, { recording: { state: 'recording', session_id: 's-live', started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754 } });
await live.goto(APP, { waitUntil: 'domcontentloaded' });
await live.waitForSelector('.notebook__controls', { timeout: 15_000 });
await live.evaluate(() => { document.documentElement.dataset.theme = 'light'; });
await live.waitForTimeout(350);
await annotate(live, 'live-recording');
await live.close();

await browser.close();
stopServer();

writeFileSync(`${OUT}/boxes.json`, JSON.stringify({ boxes, missing }, null, 2));
if (missing.length) console.log(`missing selectors: ${missing.map((m) => `#${m.id}@${m.surface}`).join(' ')}`);
