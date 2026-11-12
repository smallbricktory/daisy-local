// Re-render the shots behind the local UI review (http://127.0.0.1:3349).
//
// Drives the e2e fixture world — no build, no backend — so it captures every
// route, every Settings section, every detail tab, both themes, plus the
// design-system gallery. The surfaces come out of the DOM, so a section added
// to the app is captured without touching this file.
//
// Usage, from apps/frontend — it starts the stub dev server itself unless one
// is already listening:
//   npm run capture:review -- <out-dir>
import { mkdirSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { webkit } from 'playwright';
import { ensureDevServer, installWorld, stubMisses, walkSurfaces } from './world-stub.mjs';

const OUT = process.argv[2];
if (!OUT) throw new Error('usage: capture-review.mjs <out-dir>');
mkdirSync(OUT, { recursive: true });

const PORT = 5199;
const APP = `http://127.0.0.1:${PORT}/`;

const stopServer = await ensureDevServer(PORT);
const browser = await webkit.launch();
const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, deviceScaleFactor: 2 });
const page = await context.newPage();
await installWorld(page);
await page.goto(APP, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.rail .nav-item', { timeout: 15_000 });

const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

/** Every shot this run wrote. The manifest is built from it, so a surface
 *  that is renamed or retired cannot leave a stale image behind claiming to
 *  be current. */
const captured = new Set();

const write = async (target, name, opts = {}) => {
  await target.screenshot({ path: `${OUT}/${name}.png`, ...opts });
  captured.add(name);
  console.log(name);
};

const shot = async (name) => {
  await page.waitForTimeout(350);
  await write(page, name);
};

const SHOT_NAME = {
  route: (name) => `route-${name}`,
  settings: (name) => `settings-${slug(name)}`,
  session: () => 'session-detail',
  detail: (name) => `detail-${slug(name)}`,
};

for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  await walkSurfaces(page, async (kind, name) => {
    await shot(`${SHOT_NAME[kind](name)}-${theme}`);
  }, 500);
}

// The mini and the reminder are separate webviews, chosen by window label in
// main.tsx — nothing in the main window navigates to them, so each is captured
// in its own page booted under its own label, sized to the window Tauri gives
// it. The mini is captured idle and recording.
const SATELLITES = [
  { name: 'mini', label: 'mini', size: { width: 360, height: 260 }, overrides: {} },
  {
    name: 'mini-recording', label: 'mini', size: { width: 360, height: 260 },
    overrides: {
      recording: {
        state: 'recording',
        session_id: 's-live',
        started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754,
      },
    },
  },
  {
    name: 'reminder', label: 'reminder', size: { width: 320, height: 150 },
    overrides: { reminderTitle: 'Product sync — Q3 roadmap' },
  },
];

const satelliteMisses = [];
for (const sat of SATELLITES) {
  const satPage = await context.newPage();
  await satPage.setViewportSize(sat.size);
  await installWorld(satPage, sat.overrides, sat.label);
  await satPage.goto(APP, { waitUntil: 'domcontentloaded' });
  await satPage.waitForSelector('.mini, .reminder', { timeout: 15_000 });
  for (const theme of ['light', 'dark']) {
    await satPage.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
    await satPage.waitForTimeout(350);
    await write(satPage, `window-${sat.name}-${theme}`);
  }
  satelliteMisses.push(...await stubMisses(satPage));
  await satPage.close();
}

// The live notebook. Nothing in the rail reaches it — the app lands there off
// a recording that is already running — so it is booted from a world that has
// one, the way the satellites are.
const livePage = await context.newPage();
await livePage.setViewportSize({ width: 1280, height: 900 });
await installWorld(livePage, {
  recording: {
    state: 'recording',
    session_id: 's-live',
    started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754,
  },
});
await livePage.goto(APP, { waitUntil: 'domcontentloaded' });
await livePage.waitForSelector('.notebook__controls', { timeout: 15_000 });
for (const theme of ['light', 'dark']) {
  await livePage.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  await livePage.waitForTimeout(350);
  await write(livePage, `live-recording-${theme}`);
}
const liveMisses = await stubMisses(livePage);
await livePage.close();

const misses = [...new Set([
  ...await stubMisses(page), ...satelliteMisses, ...liveMisses,
])];

// The design-system gallery, which documents the tokens the screens use.
for (const theme of ['light', 'dark']) {
  await page.goto(`${APP}test/gallery.html?theme=${theme}`, { waitUntil: 'networkidle' });
  await page.waitForTimeout(500);
  await write(page, `gallery-${theme}`, { fullPage: true });
}

await browser.close();
stopServer();

// Shots this run did not write are from a set the app has moved past; they
// would republish as current. Drop them, then write the manifest the review
// page reads instead of a hand-synced list.
const stale = readdirSync(OUT)
  .filter((f) => f.endsWith('.png') && !captured.has(f.slice(0, -4)));
for (const f of stale) {
  rmSync(`${OUT}/${f}`);
  console.log(`dropped stale ${f}`);
}

const manifest = [...captured].sort();
writeFileSync(`${OUT}/manifest.json`, JSON.stringify(manifest, null, 2) + '\n');
console.log(`manifest.json — ${manifest.length} shots`);

if (misses.length) {
  console.error(`\n${misses.length} command(s) the fixture world cannot answer — the screens`
    + ` behind them were captured empty. Teach e2e/fixtures/world.ts:\n`
    + misses.map((c) => `  ${c}`).join('\n'));
  process.exit(1);
}
