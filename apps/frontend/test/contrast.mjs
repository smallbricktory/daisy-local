// Readability check over the rendered app, both themes.
//
// Walks every rendered element in WebKit — the engine the app ships with —
// reads the colours the engine computed with var() resolved, pairs each text
// colour with the first opaque background behind it, and reports anything
// under the WCAG AA ratio.
//
// Usage, with the stub dev server on 5199:
//   (cd apps/frontend && ./node_modules/.bin/vite --port 5199 &)
//   node test/contrast.mjs
import { webkit } from 'playwright';
import { readFileSync } from 'node:fs';

const APP = 'http://localhost:5199/';
const STUB = new URL('../../../.playwright-mcp/stub-screenshots.js', import.meta.url);
// Repo-local fixture: a bare arrow expression, not a module, so it cannot be
// imported.
const loadStub = eval(`(${readFileSync(STUB, 'utf8')})`);

/** WCAG 2.1 relative luminance. */
function luminance([r, g, b]) {
  const f = (c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

function ratio(fg, bg) {
  const [a, b] = [luminance(fg), luminance(bg)].sort((x, y) => y - x);
  return (a + 0.05) / (b + 0.05);
}

/** Collect every text-bearing element with its computed colours. */
const COLLECT = () => {
  // color-mix() resolves to color(srgb r g b / a) with 0-1 channels, so both
  // notations have to be read or every mixed colour goes unmeasured.
  const parse = (c) => {
    let m = c.match(/rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/);
    if (m) return { rgb: [+m[1], +m[2], +m[3]], a: m[4] === undefined ? 1 : +m[4] };
    m = c.match(/color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\)/);
    if (m) return { rgb: [+m[1] * 255, +m[2] * 255, +m[3] * 255], a: m[4] === undefined ? 1 : +m[4] };
    return null;
  };
  const out = [];
  for (const el of document.querySelectorAll('*')) {
    const text = [...el.childNodes]
      .filter((n) => n.nodeType === 3)
      .map((n) => n.textContent.trim())
      .join('');
    if (!text) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none' || +cs.opacity === 0) continue;
    const fg = parse(cs.color);
    if (!fg || fg.a < 0.5) continue;

    // First ancestor with an opaque-enough background is what the text sits on.
    let bg = null;
    for (let n = el; n; n = n.parentElement) {
      const s = getComputedStyle(n);
      if (s.backgroundImage && s.backgroundImage !== 'none') { bg = 'image'; break; }
      const c = parse(s.backgroundColor);
      if (c && c.a >= 0.85) { bg = c; break; }
    }
    if (!bg || bg === 'image') continue;

    out.push({
      fg: fg.rgb,
      bg: bg.rgb,
      size: parseFloat(cs.fontSize),
      weight: +cs.fontWeight || 400,
      cls: el.className && typeof el.className === 'string' ? el.className : el.tagName.toLowerCase(),
      text: text.slice(0, 48),
    });
  }
  return out;
};

const browser = await webkit.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
await loadStub(page);
await page.waitForTimeout(800);

const VIEWS = [
  ['library', async () => {}],
  ['detail', async () => {
    await page.getByText('Product sync — Q3 roadmap').first().click();
    await page.waitForTimeout(700);
  }],
  ['transcript', async () => {
    await page.locator('.detail-tabs__tab', { hasText: 'transcript' }).first().click();
    await page.waitForTimeout(600);
  }],
  ['participants', async () => {
    await page.locator('.detail-tabs__tab', { hasText: 'participants' }).first().click();
    await page.waitForTimeout(600);
  }],
];

const failures = [];
const seen = new Set();
let checked = 0;

for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  for (const [name, go] of VIEWS) {
    await go();
    await page.waitForTimeout(200);
    for (const s of await page.evaluate(COLLECT)) {
      checked++;
      const large = s.size >= 24 || (s.size >= 18.66 && s.weight >= 700);
      const need = large ? 3.0 : 4.5;
      const r = ratio(s.fg, s.bg);
      if (r >= need) continue;
      const key = `${theme}|${s.cls}|${Math.round(r * 10)}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const hex = (c) => '#' + c.map((v) => Math.round(v).toString(16).padStart(2, '0')).join('');
      failures.push(
        `  ${theme}/${name}  ${r.toFixed(2)}:1 (needs ${need})  ${hex(s.fg)} on ${hex(s.bg)}` +
        `\n      .${s.cls}  "${s.text}"`,
      );
    }
  }
}

await browser.close();
console.log(`inventoried ${checked} text elements across ${VIEWS.length} views x 2 themes`);
if (failures.length) {
  console.error(`\n${failures.length} unreadable:\n${failures.join('\n')}`);
  process.exit(1);
}
console.log('all text meets WCAG AA against its actual background');
