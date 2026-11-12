// Readability check over the rendered app, both themes.
//
// Walks every rendered element in WebKit — the engine the app ships with —
// reads the colours the engine computed with var() resolved, pairs each text
// colour with the first opaque background behind it, and reports anything
// under the WCAG AA ratio.
//
// The surfaces are enumerated from the DOM: every rail item, every Settings
// section, every detail tab. A section added to the app is swept without
// touching this file. `EXPECTED_SURFACES` is the other half of that — a
// surface that stops rendering fails the run instead of quietly leaving the
// sweep.
//
// Usage, from apps/frontend — it starts the stub dev server itself unless one
// is already listening:
//   node test/contrast.mjs
import { readFileSync } from 'node:fs';
import { webkit } from 'playwright';
import { ensureDevServer, installWorld, stubMisses, walkSurfaces, walkWizardToNames, walkWizardToPlan } from './world-stub.mjs';

const PORT = 5199;
const APP = `http://127.0.0.1:${PORT}/`;

// The surfaces this sweep must reach, and the chrome that carries the glass —
// shared with the static rules so neither list can drift from the other.
const SURFACES = JSON.parse(readFileSync(new URL('design-surfaces.json', import.meta.url), 'utf8'));
const EXPECTED_SURFACES = SURFACES.surfaces;

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
  const parse = window.readColour;
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

    // What the text sits on: the first opaque ancestor background, with every
    // translucent one above it composited back down over it. Chrome is glass,
    // so the ground under a menu label is a blend, not the page under it.
    let bg = null;
    const sheer = [];
    for (let n = el; n; n = n.parentElement) {
      const s = getComputedStyle(n);
      if (s.backgroundImage && s.backgroundImage !== 'none') { bg = 'image'; break; }
      const c = parse(s.backgroundColor);
      if (!c || c.a === 0) continue;
      if (c.a >= 0.999) { bg = c; break; }
      sheer.push(c);
    }
    if (!bg || bg === 'image') continue;
    for (let i = sheer.length - 1; i >= 0; i--) {
      const l = sheer[i];
      bg = { rgb: bg.rgb.map((v, k) => l.rgb[k] * l.a + v * (1 - l.a)), a: 1 };
    }

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

const stopServer = await ensureDevServer(PORT);
const browser = await webkit.launch();
const context = await browser.newContext({ viewport: { width: 1280, height: 900 } });

// One colour reader for rgb() and color(srgb …) notations.
await context.addInitScript(() => {
  window.readColour = (c) => {
    let m = c.match(/rgba?\(([\d.]+),\s*([\d.]+),\s*([\d.]+)(?:,\s*([\d.]+))?\)/);
    if (m) return { rgb: [+m[1], +m[2], +m[3]], a: m[4] === undefined ? 1 : +m[4] };
    m = c.match(/color\(srgb\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)(?:\s*\/\s*([\d.]+))?\)/);
    if (m) return { rgb: [+m[1] * 255, +m[2] * 255, +m[3] * 255], a: m[4] === undefined ? 1 : +m[4] };
    return null;
  };
});

// The probes that ask what a scroll does all need the page taller than the
// window first, and all have to leave it as they found it.
await context.addInitScript(() => {
  window.growPage = (px = 1500) => {
    const body = document.querySelector('.page__body');
    if (!body) return null;
    const spacer = document.createElement('div');
    spacer.style.height = `${px}px`;
    body.appendChild(spacer);
    return () => spacer.remove();
  };
  // How far below the header band something sits: negative means the band is
  // over it.
  window.underBand = (el) => {
    const band = document.querySelector('.page-header');
    return band ? Math.round(el.getBoundingClientRect().top - band.getBoundingClientRect().bottom) : null;
  };
});

let page = await context.newPage();
await installWorld(page);
await page.goto(APP, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.rail .nav-item', { timeout: 15_000 });

const failures = [];
const seen = new Set();
const swept = new Set();
let checked = 0;

/** Point the sweep at another page for the length of `fn`. The measurements
 *  read `page`, and the satellite webviews and the empty worlds are each their
 *  own page. */
async function sweeping(p, fn) {
  const outer = page;
  page = p;
  try { await fn(); } finally { page = outer; }
}

/** Filled-accent buttons painted on each surface: clickable elements filled with the gold or the recording red.
 *  The rail and switches in the on position are excluded. Counted on the light pass only. */
const accents = new Map();

const ACCENT_FILLS = ['--gold', '--record-fill'];

async function countAccents(name) {
  const found = await page.evaluate((fills) => {
    const root = getComputedStyle(document.documentElement);
    const accent = new Set(fills.map((v) => {
      const probe = document.createElement('span');
      probe.style.backgroundColor = root.getPropertyValue(v).trim();
      document.body.append(probe);
      const painted = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return painted;
    }));
    const clickable = 'button, a, [role="button"], summary, input[type="submit"]';
    const state = (el) => el.matches('[role="switch"], [role="checkbox"]') || el.closest('.switch') !== null;
    return [...document.querySelectorAll(clickable)]
      .filter((el) => el.closest('.rail') === null && !state(el))
      .filter((el) => accent.has(getComputedStyle(el).backgroundColor))
      .filter((el) => {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0) return false;
        if (getComputedStyle(el).visibility === 'hidden') return false;
        // A closed <details> keeps a layout box for its skipped contents; hit-testing decides what is on screen.
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return hit === el || el.contains(hit);
      })
      .map((el) => `${el.dataset.testid ?? el.className}: ${(el.textContent ?? '').trim().slice(0, 30)}`);
  }, ACCENT_FILLS);
  accents.set(name, found);
}

/** Where each route's page header puts its title and its actions. Measured on the light pass. */
const headers = new Map();

async function measureHeader(name) {
  const geom = await page.evaluate(() => {
    const hs = [...document.querySelectorAll('.page-header')];
    if (hs.length !== 1) return { count: hs.length };
    const h = hs[0];
    const title = h.querySelector('.page-header__title');
    const actions = h.querySelector('.page-header__actions');
    return {
      count: 1,
      titleLeft: title ? Math.round(title.getBoundingClientRect().left) : null,
      actionsRight: actions ? Math.round(actions.getBoundingClientRect().right) : null,
    };
  });
  headers.set(name, geom);
}

/** Header lock: on a document route the page is the one scroller with the header sticky inside it.
 *  Measured by growing the body past the window and scrolling, and by scrolling a row into view.
 *  A pane route (`.page--bleed`) scrolls through its panes and is measured for that. */
const STEP = 500;
const locks = new Map();

/** What a pinned thing has to say for itself: flush under the band, or the
 *  two ways it can be wrong. */
function bandGap(what, gap) {
  if (gap === null) return [`${what} has no header band to pin against`];
  if (gap < -1) return [`${what} sits ${-gap}px behind the header band`];
  if (gap > 1) return [`${what} stops ${gap}px below the band, with the page scrolling through the gap`];
  return [];
}

async function measureLock(name) {
  const m = await page.evaluate((step) => {
    const el = document.querySelector('.page');
    if (!el) return null;
    const header = el.querySelector('.page-header');
    // A box with `overflow: hidden` still answers scrollTop from script, so
    // what the wheel does is read off the computed value, not off the move.
    const overflow = getComputedStyle(el).overflowY;
    // The page is the one scroller: the shell it sits in must have stopped
    // being one, or the wheel never reaches the page at all.
    const shell = el.closest('.main__body');
    const shellScrolls = shell ? /^(auto|scroll)$/.test(getComputedStyle(shell).overflowY) : false;
    if (el.classList.contains('page--bleed')) {
      el.scrollTop = step;
      const scrolled = Math.round(el.scrollTop);
      el.scrollTop = 0;
      return { bleed: true, overflow, scrolled, shellScrolls, header: !!header };
    }
    const body = el.querySelector('.page__body');
    if (!header || !body) return { bleed: false, overflow, shellScrolls, missing: !header ? 'header' : 'body' };
    const ungrow = window.growPage();
    const headerBefore = header.getBoundingClientRect().top;
    const bodyBefore = body.getBoundingClientRect().top;
    el.scrollTop = step;
    const m = {
      bleed: false,
      overflow,
      shellScrolls,
      scrolled: Math.round(el.scrollTop),
      headerMoved: Math.round(Math.abs(header.getBoundingClientRect().top - headerBefore)),
      bodyMoved: Math.round(bodyBefore - body.getBoundingClientRect().top),
    };
    // Below the spacer so it can only be reached by scrolling, and with room
    // under it so the scroll is not cut short by the end of the page.
    const probe = document.createElement('div');
    probe.style.height = '24px';
    body.appendChild(probe);
    const tail = document.createElement('div');
    tail.style.height = '1500px';
    body.appendChild(tail);
    el.scrollTop = 0;
    probe.scrollIntoView({ block: 'start' });
    m.covered = -window.underBand(probe);
    el.scrollTop = 0;
    probe.remove();
    tail.remove();
    ungrow();
    return m;
  }, STEP);
  if (m) locks.set(name, m);
}

/** Row titles the library clips at the default column width. Light pass only. */
let clippedTitles = null;

async function measureLibraryTitles() {
  clippedTitles = await page.evaluate(() => [...document.querySelectorAll('.session-row__name')]
    .filter((el) => el.scrollWidth > el.clientWidth + 1)
    .map((el) => (el.textContent ?? '').trim()));
}

/** Rows whose centre is claimed by a chip or control inside them. */
let stolenRowCentres = null;

async function measureRowHitTargets() {
  stolenRowCentres = await page.evaluate(() => [...document.querySelectorAll('.session-row')]
    .map((row) => {
      const r = row.getBoundingClientRect();
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      if (!hit || !row.contains(hit)) return `${row.dataset.testid ?? 'row'}: centre is outside the row`;
      // Anything that carries its own click handler owns that pixel instead.
      const thief = hit.closest('button, a, input, [data-testid^="library-ghosts-"]');
      return thief && row.contains(thief)
        ? `${row.dataset.testid ?? 'row'}: centre lands on ${thief.dataset.testid ?? thief.tagName.toLowerCase()}`
        : null;
    })
    .filter((x) => x !== null));
}

/** The recorder's balance: transcript size against the notes box, notes growth on focus, and the recording edge. Light pass only. */
let recorder = null;

async function measureRecorder() {
  const heights = () => page.evaluate(() => {
    const h = (sel) => {
      const el = document.querySelector(sel);
      return el ? Math.round(el.getBoundingClientRect().height) : null;
    };
    const edge = document.querySelector('.rec-edge');
    return {
      transcript: h('.notebook__transcript'),
      notes: h('.notebook__notes'),
      edge: edge
        ? { height: Math.round(edge.getBoundingClientRect().height), paint: getComputedStyle(edge).backgroundColor }
        : null,
    };
  });
  const rest = await heights();
  await page.locator('[data-testid="live-notes"]').focus();
  await page.waitForTimeout(500);
  const focused = await heights();
  await page.evaluate(() => document.activeElement?.blur());
  await page.waitForTimeout(500);
  recorder = { ...rest, notesFocused: focused.notes };
}

/** What a container says while it holds nothing. Light pass only. */
const empties = new Map();

async function measureEmpty(name) {
  empties.set(name, await page.evaluate(() => {
    const blocks = [...document.querySelectorAll('[data-empty]')];
    const step = document.querySelector(
      '.empty__action button, [data-testid="search-query"],'
      + ' [data-testid="session-chapters-extract"]',
    );
    return {
      count: blocks.length,
      title: blocks[0]?.querySelector('.empty__title')?.textContent?.trim() ?? '',
      body: blocks[0]?.querySelector('.empty__body')?.textContent?.trim() ?? '',
      step: step ? (step.dataset.testid ?? step.textContent.trim()) : null,
    };
  }));
}

/** Stepper parts and the fields of a form column. Light pass only. */
const steppers = [];
const fieldWidths = new Map();
let measuredSteppers = 0;

async function measureStepper(name) {
  const gaps = await page.evaluate(() => [...document.querySelectorAll('.stepper')].flatMap((g) => {
    const parts = [...g.children].map((c) => c.getBoundingClientRect());
    return parts.slice(1).map((r, i) => Math.round(r.left - parts[i].right));
  }));
  measuredSteppers += gaps.length ? 1 : 0;
  for (const gap of gaps) if (gap > 0) steppers.push(`${name}: stepper parts ${gap}px apart`);
}

async function measureFields(name) {
  const widths = await page.evaluate(() => [...document.querySelectorAll('.field__control')]
    .map((el) => Math.round(el.getBoundingClientRect().width)));
  if (widths.length) fieldWidths.set(name, widths);
}

/** Measures the character advance of each block's own font against its line length. */
const longLines = [];

async function measureProse(name) {
  const over = await page.evaluate(() => {
    const c = document.createElement('canvas').getContext('2d');
    const out = [];
    for (const el of document.querySelectorAll('p, li, blockquote')) {
      const text = el.textContent.trim();
      if (text.length < 120) continue;
      const cs = getComputedStyle(el);
      if (cs.display === 'none' || cs.visibility === 'hidden') continue;
      const w = el.getBoundingClientRect().width;
      if (w < 40) continue;
      c.font = `${cs.fontStyle} ${cs.fontWeight} ${cs.fontSize} ${cs.fontFamily}`;
      const advance = c.measureText(text).width / text.length;
      const chars = Math.round(w / advance);
      if (chars > 100) out.push({ chars, text: text.slice(0, 60) });
    }
    return out;
  });
  for (const o of over) longLines.push(`${name}: ${o.chars} characters a line — "${o.text}…"`);
}

/** The record screen measured at the window's minimum size, then restored. */
const SHORT = { width: 1100, height: 720 };
let fold = null;

async function measureFold() {
  await page.setViewportSize(SHORT);
  await page.waitForTimeout(300);
  fold = await page.evaluate((h) => {
    const card = document.querySelector('.record-card');
    if (!card) return null;
    const r = card.getBoundingClientRect();
    const start = document.querySelector('[data-testid="record-start"]');
    return {
      bottom: Math.round(r.bottom),
      start: start ? Math.round(start.getBoundingClientRect().bottom) : null,
      window: h,
    };
  }, SHORT.height);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.waitForTimeout(300);
}

/** The copy control's name and right edge on each meeting tab. */
const copyControls = new Map();

async function measureCopy(name) {
  const m = await page.evaluate(() => {
    const copy = document.querySelector('[data-testid^="session-copy-"]');
    const action = document.querySelector('[data-testid="session-actions"]');
    if (!copy || !action) return null;
    return {
      label: copy.textContent.trim(),
      right: Math.round(copy.getBoundingClientRect().right),
      actionRight: Math.round(action.getBoundingClientRect().right),
      top: Math.round(copy.getBoundingClientRect().top),
      actionTop: Math.round(action.getBoundingClientRect().top),
    };
  });
  if (m) copyControls.set(name, m);
}

/** A table of runs: mono timestamps, and titles that underline on hover only. */
const tables = new Map();

async function measureTable(name) {
  const m = await page.evaluate(() => {
    const table = document.querySelector('.data-table');
    if (!table) return null;
    const when = [...table.querySelectorAll('.data-table__when')].map((el) => {
      const cs = getComputedStyle(el);
      return { mono: cs.fontFamily.includes('JetBrains Mono'), nums: cs.fontVariantNumeric };
    });
    const underlined = [...table.querySelectorAll('.btn-link, a')]
      .filter((el) => getComputedStyle(el).textDecorationLine.includes('underline')).length;
    return { rows: table.querySelectorAll('tbody tr').length, when, underlined };
  });
  if (m) tables.set(name, m);
}

/** A list row's own control against the text it acts on, within the page measure. */
const MEASURE_MAX = 900;
const rows = new Map();

async function measureRows(name) {
  const m = await page.evaluate(() => [...document.querySelectorAll('.event-card')].map((el) => {
    const r = el.getBoundingClientRect();
    const ctl = el.querySelector('[data-testid^="calendar-dismiss-"]');
    const text = el.querySelector('.event-card__title');
    return {
      width: Math.round(r.width),
      gap: ctl && text
        ? Math.round(ctl.getBoundingClientRect().left - text.getBoundingClientRect().right)
        : null,
    };
  }));
  if (m.length) rows.set(name, m);
}

/** Saved turns carry a time. */
let turns = null;

async function measureTurns() {
  turns = await page.evaluate(() => {
    const attributed = [...document.querySelectorAll('.turn.me, .turn.them')];
    return {
      count: attributed.length,
      timed: attributed.filter((t) => t.querySelector('.turn-ts')).length,
    };
  });
}

/** Nested corner radii against the gap between them. */
const corners = [];

async function measureCorners(name) {
  const bad = await page.evaluate(() => {
    const radius = (el) => parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
    const label = (el) => `${el.tagName.toLowerCase()}${el.className && typeof el.className === 'string'
      ? '.' + el.className.trim().split(/\s+/).slice(0, 2).join('.') : ''}`;
    const out = [];
    for (const el of document.querySelectorAll('*')) {
      const r = radius(el);
      if (r <= 0 || r > 100) continue;
      let parent = el.parentElement;
      while (parent && radius(parent) <= 0) parent = parent.parentElement;
      if (!parent || radius(parent) > 100) continue;
      const pr = radius(parent);
      const a = el.getBoundingClientRect();
      const b = parent.getBoundingClientRect();
      const gap = Math.round(Math.min(a.left - b.left, a.top - b.top));
      if (gap < 0 || gap > 12) continue;
      const ideal = Math.max(0, pr - gap);
      if (Math.abs(r - ideal) <= 4) continue;
      out.push(`${label(el)} ${r}px inside ${label(parent)} ${pr}px, gap ${gap}px (concentric: ${ideal}px)`);
    }
    return out;
  });
  for (const b of bad) corners.push(`${name}: ${b}`);
}

/** Every picker is opened, measured and closed: menu surface, coverage, width and option contrast. */
const pickers = [];
let pickersSeen = 0;

/** Glass: the rail, the page-header band, menus and the toast stack are translucent; content is opaque.
 *  Which chrome also blurs is derived from whether content can sit behind it. */
const CHROME = SURFACES.chrome;
const glass = [];
let glassSeen = 0;

async function measureGlass(name) {
  const found = await page.evaluate((chrome) => {
    const problems = [];
    let seen = 0;
    for (const el of document.querySelectorAll(chrome.join(','))) {
      const cs = getComputedStyle(el);
      const r = el.getBoundingClientRect();
      if (r.width < 4 || r.height < 4) continue;
      seen++;
      const label = `${el.tagName.toLowerCase()}${typeof el.className === 'string' && el.className
        ? '.' + el.className.trim().split(/\s+/)[0] : ''}`;
      if ((window.readColour(cs.backgroundColor)?.a ?? 1) > 0.999) {
        problems.push(`${label} is chrome painting opaque — the page tints through it`);
      }
      // Content already inside the box.
      const covers = [0.25, 0.5, 0.75].some((f) => document
        .elementsFromPoint(r.left + r.width * f, r.top + r.height / 2)
        .some((o) => !el.contains(o) && !o.contains(el)));
      // Or content that a scroll can bring under it.
      let pinned = cs.position === 'sticky' || cs.position === 'fixed';
      if (pinned) {
        pinned = false;
        for (let a = el.parentElement; a && a !== document.body; a = a.parentElement) {
          if (/^(auto|scroll)$/.test(getComputedStyle(a).overflowY)) { pinned = true; break; }
        }
      }
      const behind = covers || pinned;
      const blurs = cs.backdropFilter !== 'none' && cs.backdropFilter !== '';
      if (behind && !blurs) problems.push(`${label} has the page passing behind it and paints flat`);
      if (!behind && blurs) problems.push(`${label} blurs, and nothing passes behind it`);
    }
    // A blur anywhere but on chrome.
    for (const el of document.querySelectorAll('*')) {
      const cs = getComputedStyle(el);
      if (cs.backdropFilter === 'none' || cs.backdropFilter === '') continue;
      if (chrome.some((c) => el.matches(c))) continue;
      problems.push(`${el.tagName.toLowerCase()}.${String(el.className).trim().split(/\s+/)[0]}`
        + ' blurs what is behind it — glass is chrome');
    }
    return { problems, seen };
  }, CHROME);
  for (const t of found.problems) glass.push(`${name}: ${t}`);
  glassSeen += found.seen > 0 && found.problems.length === 0 ? 1 : 0;
}

async function measurePickers(name) {
  const count = await page.evaluate(
    () => document.querySelectorAll('.select__trigger:not(:disabled)').length,
  );
  for (let i = 0; i < count; i++) {
    const trigger = page.locator('.select__trigger:not(:disabled)').nth(i);
    await trigger.evaluate((el) => el.click());
    await page.waitForTimeout(80);
    const p = await trigger.evaluate((el) => {
      const parse = (c) => window.readColour(c)?.rgb ?? null;
      const label = el.dataset.testid || el.id || 'a picker';
      const pop = el.parentElement.querySelector('[role="listbox"]');
      if (!pop) return { label, bad: 'opened no list' };
      const cs = getComputedStyle(pop);
      const box = pop.getBoundingClientRect();
      const opts = [...pop.querySelectorAll('[role="option"]')];
      return {
        label,
        bg: parse(cs.backgroundColor),
        material: cs.backdropFilter !== 'none' && cs.backdropFilter !== '',
        fg: opts.length ? parse(getComputedStyle(opts[0]).color) : null,
        text: opts.length ? opts[0].textContent.trim().slice(0, 40) : '',
        options: opts.length,
        stacked: cs.position !== 'static' && (parseInt(cs.zIndex, 10) || 0) > 0,
        width: Math.round(box.width),
        control: Math.round(el.getBoundingClientRect().width),
      };
    });
    await trigger.evaluate((el) => { if (el.getAttribute('aria-expanded') === 'true') el.click(); });
    await page.waitForTimeout(40);
    pickersSeen++;
    const where = `${name}: ${p.label}`;
    if (p.bad) { pickers.push(`${where} ${p.bad}`); continue; }
    if (p.options === 0) { pickers.push(`${where} opened an empty list`); continue; }
    if (p.bg[3] < 1 && !p.material) {
      pickers.push(`${where} draws its list on a see-through surface`);
    }
    if (!p.stacked) pickers.push(`${where} draws its list in the flow, under what it covers`);
    if (p.width < p.control - 1) {
      pickers.push(`${where} list is ${p.width}px under a ${p.control}px control`);
    }
    if (p.fg) {
      const r = ratio(p.fg, p.bg);
      if (r < 4.5) pickers.push(`${where} option "${p.text}" reads ${r.toFixed(2)}:1`);
    }
  }
}

/** Measure whatever is on screen now, under `name`. */
async function measure(theme, name) {
  swept.add(name);
  await page.waitForTimeout(250);
  // Geometry, material and colour counts read the same in either theme, so
  // they are measured on the light pass alone. Contrast is measured on both.
  if (theme === 'light') {
    await countAccents(name);
    if (name.startsWith('route:')) {
      await measureHeader(name);
      await measureLock(name);
    }
    if (name === 'route:library') {
      await measureLibraryTitles();
      await measureRowHitTargets();
    }
    if (name === 'state:live-recording') await measureRecorder();
    if (name === 'route:record') await measureFold();
    if (name.startsWith('detail:')) await measureCopy(name);
    if (name === 'route:history') await measureTable(name);
    if (name === 'route:calendar') await measureRows(name);
    if (name === 'detail:transcript') await measureTurns();
    await measureGlass(name);
    await measurePickers(name);
    await measureCorners(name);
    if (name.startsWith('empty:')) await measureEmpty(name);
    await measureStepper(name);
    await measureFields(name);
    await measureProse(name);
  }
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

// Every surface the app reaches on its own, in both themes. The session as it
// opens shows its first detail tab, so it is measured under that tab's name
// rather than twice.
for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  await walkSurfaces(page, async (kind, name) => {
    if (kind !== 'session') await measure(theme, `${kind}:${name}`);
  });
}

// ===== The Nerd Mode badge =====
//
// The badge rides the Settings header while the mode is on, which is the
// fixture world's state. It is measured under its own name so a badge that
// stops rendering fails the run.
await page.locator('[data-testid="nav-settings"]').first().click();
await page.waitForSelector('[data-testid="settings-nerd-badge"]', { timeout: 10_000 });
for (const theme of ['light', 'dark']) {
  await page.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
  await measure(theme, 'badge:nerd');
}
await page.evaluate(() => { document.documentElement.dataset.theme = 'light'; });

// ===== The windows the rail cannot reach =====
//
// The mini and the reminder are separate webviews, chosen in main.tsx by
// window label, or by a state the rail has no item for. Nothing navigates to
// any of them, so each is swept in its own page booted under its own label and
// world. The mini is swept twice: idle, and recording — the state that paints
// --record and the pulsing dot.
const satellites = [
  // The live notebook: same window as the app, but the rail never reaches it
  // — the app lands there off a recording that is already running.
  {
    name: 'state:live-recording',
    label: 'main',
    ready: '.notebook__controls',
    overrides: {
      recording: {
        state: 'recording',
        session_id: 's-live',
        started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754,
      },
    },
  },
  { name: 'window:mini', label: 'mini', overrides: {} },
  {
    name: 'window:mini-recording',
    label: 'mini',
    overrides: {
      recording: {
        state: 'recording',
        session_id: 's-live',
        started_at_unix_seconds: Math.floor(Date.now() / 1000) - 754,
      },
    },
  },
  { name: 'window:reminder', label: 'reminder', overrides: { reminderTitle: 'Product sync — Q3 roadmap' } },
  { name: 'window:reminder-meeting', label: 'reminder', overrides: { reminderKind: 'meeting' } },
  { name: 'window:reminder-meeting-ended', label: 'reminder', overrides: { reminderKind: 'meeting_ended' } },
  // The setup steps after the welcome: pre-main, so the rail never reaches
  // them. A world with no bootstrap and no vault boots the app straight into
  // the wizard.
  {
    name: 'wizard:names',
    label: 'main',
    ready: '[data-testid="wizard-get-started"]',
    overrides: { phase: { eula: true, consent: true, vaultExists: false, unlocked: false } },
    walk: walkWizardToNames,
  },
  {
    name: 'wizard:plan',
    label: 'main',
    ready: '[data-testid="wizard-get-started"]',
    overrides: { phase: { eula: true, consent: true, vaultExists: false, unlocked: false } },
    walk: walkWizardToPlan,
  },
];

const satelliteMisses = [];
for (const sat of satellites) {
  const satPage = await context.newPage();
  await installWorld(satPage, sat.overrides, sat.label);
  await satPage.goto(APP, { waitUntil: 'domcontentloaded' });
  await satPage.waitForSelector(sat.ready ?? '.mini, .reminder', { timeout: 15_000 });
  if (sat.walk) await sat.walk(satPage);
  await sweeping(satPage, async () => {
    for (const theme of ['light', 'dark']) {
      await satPage.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
      await measure(theme, sat.name);
    }
  });
  satelliteMisses.push(...await stubMisses(satPage));
  await satPage.close();
}

// Every container the app fills from the backend is swept a second time against a world that returns nothing.
const EMPTY_WORLD = {
  workflows: [], runs: [], queue: [], integrationRuns: [], events: [],
  chats: {}, chapters: {},
};

const emptyPage = await context.newPage();
await installWorld(emptyPage, EMPTY_WORLD, 'main');
await emptyPage.goto(APP, { waitUntil: 'domcontentloaded' });
await emptyPage.waitForSelector('.rail', { timeout: 15_000 });

const clickIn = async (sel) => {
  await emptyPage.locator(sel).first().click();
  await emptyPage.waitForTimeout(400);
};
const emptySteps = [
  ['empty:calendar', () => clickIn('[data-testid="nav-calendar"]')],
  ['empty:search', async () => {
    await clickIn('[data-testid="nav-search"]');
    await emptyPage.locator('[data-testid="search-query"]').fill('zzqq-no-such-meeting');
    await emptyPage.waitForSelector('[data-testid="search-empty"]', { timeout: 10_000 });
  }],
  ['empty:history', () => clickIn('[data-testid="nav-history"]')],
  ['empty:workflows', () => clickIn('[data-testid="nav-tasks"]')],
  ['empty:chapters', async () => {
    await clickIn('[data-testid="nav-library"]');
    await emptyPage.getByText('Product sync — Q3 roadmap').first().click();
    await emptyPage.waitForSelector('.detail-tabs__tab', { timeout: 10_000 });
    await clickIn('[data-testid="session-tab-transcript"]');
  }],
];

await sweeping(emptyPage, async () => {
  for (const theme of ['light', 'dark']) {
    await emptyPage.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
    for (const [name, go] of emptySteps) {
      await go();
      await measure(theme, name);
    }
  }
});
satelliteMisses.push(...await stubMisses(emptyPage));
await emptyPage.close();

// A profile that has never recorded: the two routes built around the meeting
// list have nothing to list.
const freshPage = await context.newPage();
await installWorld(freshPage, { ...EMPTY_WORLD, sessions: [], suggestions: {} }, 'main');
await freshPage.goto(APP, { waitUntil: 'domcontentloaded' });
await freshPage.waitForSelector('.rail', { timeout: 15_000 });
await sweeping(freshPage, async () => {
  for (const theme of ['light', 'dark']) {
    await freshPage.evaluate((t) => { document.documentElement.dataset.theme = t; }, theme);
    for (const [name, key] of [['empty:library', 'library'], ['empty:analyzer', 'analyzer']]) {
      await freshPage.locator(`[data-testid="nav-${key}"]`).first().click();
      await freshPage.waitForTimeout(400);
      await measure(theme, name);
    }
  }
});
satelliteMisses.push(...await stubMisses(freshPage));
await freshPage.close();

// Increase Contrast, and the attribute the app writes for macOS Reduce Transparency and for Linux, are measured.
// Chromium's prefers-reduced-transparency is covered by the static rule in ui-consistency.
const glassOff = [];
const materialOf = async () => page.evaluate((chrome) => Object.fromEntries(
  [...document.querySelectorAll(chrome.join(','))]
    .filter((el) => el.getBoundingClientRect().width > 4)
    .map((el) => {
      const cs = getComputedStyle(el);
      return [`.${String(el.className).trim().split(/\s+/)[0]}`,
        { blur: cs.backdropFilter, a: window.readColour(cs.backgroundColor)?.a ?? 1 }];
    }),
), CHROME);
{
  // Every chrome surface on screen, measured under each setting.
  const on = await materialOf();
  const names = Object.keys(on);
  if (names.length === 0) glassOff.push('no chrome surface was on screen to measure');
  for (const [what, m] of Object.entries(on)) {
    if (m.a > 0.999) glassOff.push(`${what} is not translucent to begin with`);
  }
  for (const [what, apply, undo] of [
    ['Increase Contrast',
      () => page.emulateMedia({ contrast: 'more' }),
      () => page.emulateMedia({ contrast: 'no-preference' })],
    ['the app attribute (macOS Reduce Transparency, Linux)',
      () => page.evaluate(() => { document.documentElement.dataset.reduceTransparency = '1'; }),
      () => page.evaluate(() => { delete document.documentElement.dataset.reduceTransparency; })],
  ]) {
    await apply();
    await page.waitForTimeout(120);
    for (const [surface, m] of Object.entries(await materialOf())) {
      if (m.blur !== 'none') glassOff.push(`${what} leaves ${surface} blurring (${m.blur})`);
      if (m.a <= 0.999) glassOff.push(`${what} leaves ${surface} see-through`);
    }
    await undo();
    await page.waitForTimeout(120);
  }
}

// A route's notice stays pinned under the band while the list scrolls. Measured on Calendar.
const noticePin = [];
{
  await page.locator('[data-testid="nav-calendar"]').first().click();
  await page.waitForSelector('[data-testid="calendar-sync"]', { timeout: 10_000 });
  await page.locator('[data-testid="calendar-sync"]').first().click();
  await page.locator('[data-testid="calendar-note"]').first().waitFor({ timeout: 10_000 });
  const pin = await page.evaluate((step) => {
    const el = document.querySelector('.page');
    const notice = document.querySelector('[data-testid="calendar-note"]');
    const ungrow = window.growPage();
    if (!ungrow || !notice) return { missing: true };
    const before = notice.getBoundingClientRect().top;
    el.scrollTop = step;
    const out = {
      moved: Math.round(Math.abs(notice.getBoundingClientRect().top - before)),
      scrolled: Math.round(el.scrollTop),
      underBand: window.underBand(notice),
    };
    el.scrollTop = 0;
    ungrow();
    return out;
  }, STEP);
  // A pinned notice settles the short distance to the band and stops there;
  // an unpinned one travels the whole scroll and leaves the screen.
  if (pin.missing) noticePin.push('Calendar paints no notice to pin');
  else if (pin.scrolled !== STEP) noticePin.push(`the page scrolled ${pin.scrolled}px of ${STEP}px, so the pin is unmeasured`);
  else if (pin.moved >= STEP) noticePin.push(`the notice slid ${pin.moved}px with the body instead of holding under the band`);
  else noticePin.push(...bandGap('the notice', pin.underBand));
}

// The band keeps its x whether or not the page overflows. The scrollbar is given a width for the measurement.
const gutter = [];
{
  const g = await page.evaluate(() => {
    const band = () => {
      const h = document.querySelector('.page-header');
      const actions = h.querySelector('.page-header__actions');
      return {
        right: Math.round(h.getBoundingClientRect().right),
        actionsRight: actions ? Math.round(actions.getBoundingClientRect().right) : null,
      };
    };
    const style = document.createElement('style');
    style.textContent = '*::-webkit-scrollbar { width: 15px; height: 15px }'
      + '*::-webkit-scrollbar-thumb { background: #888 }';
    document.head.appendChild(style);
    const fits = band();
    const ungrow = window.growPage();
    const page = document.querySelector('.page');
    const grew = ungrow !== null && page.scrollHeight > page.clientHeight;
    const overflows = band();
    ungrow?.();
    style.remove();
    return { fits, overflows, grew };
  });
  if (!g.grew) gutter.push('the page did not overflow, so the gutter is unmeasured');
  else if (g.overflows.actionsRight === null) gutter.push('the route measured carries no header actions, so half the gutter is unmeasured');
  else {
    for (const [field, what] of [['right', "the band's right edge"], ['actionsRight', 'the header actions']]) {
      const moved = g.fits[field] - g.overflows[field];
      if (moved !== 0) gutter.push(`${what} moved ${moved}px when the page overflowed`);
    }
  }
}

// A Settings section opens at its top: the reset moves the page.
const sectionReset = [];
const navPin = [];
{
  await page.locator('[data-testid="nav-settings"]').first().click();
  await page.waitForSelector('.settings-tab', { timeout: 10_000 });
  const tabs = await page.evaluate(() => [...document.querySelectorAll('.settings-tab')]
    .map((b) => b.textContent.trim()));
  const toBottom = () => page.evaluate(() => {
    const el = document.querySelector('.page');
    if (!el) return null;
    el.scrollTop = 10_000;
    return Math.round(el.scrollTop);
  });
  // Both sections are long enough to hold an offset.
  const tall = [];
  for (const tab of tabs) {
    await page.locator('.settings-tab', { hasText: tab }).first().click();
    await page.waitForTimeout(250);
    if ((await toBottom()) > 0) tall.push(tab);
    if (tall.length === 2) break;
  }
  if (tall.length < 2) {
    sectionReset.push(`only ${tall.length} Settings section(s) are long enough to scroll,`
      + ' so the reset is unmeasured');
  } else {
    const [to, from] = tall;
    await page.locator('.settings-tab', { hasText: from }).first().click();
    await page.waitForTimeout(250);
    const fromTop = await toBottom();
    // The section list is sticky inside the same scroller as the header and carries the header's height as an offset.
    // Read where the two land while the page is scrolled.
    const gap = await page.evaluate(() => {
      const list = document.querySelector('.page__body nav');
      return list ? window.underBand(list) : undefined;
    });
    if (gap === undefined) navPin.push('Settings paints no section list to pin');
    else navPin.push(...bandGap('the section list', gap));
    await page.locator('.settings-tab', { hasText: to }).first().click();
    await page.waitForTimeout(300);
    const opened = await page.evaluate(() => Math.round(document.querySelector('.page').scrollTop));
    if (opened !== 0) {
      sectionReset.push(`"${to}" opened at ${opened}px after "${from}" was left at ${fromTop}px`);
    }
    // Picking the section that is already open returns it to the top.
    const left = await toBottom();
    await page.locator('.settings-tab', { hasText: to }).first().click();
    await page.waitForTimeout(300);
    const repicked = await page.evaluate(() => Math.round(document.querySelector('.page').scrollTop));
    if (repicked !== 0) {
      sectionReset.push(`re-picking "${to}" left it at ${repicked}px after it was scrolled to ${left}px`);
    }
  }
}

const misses = [...new Set([...await stubMisses(page), ...satelliteMisses])];
await browser.close();
stopServer();

console.log(`inventoried ${checked} text elements across ${swept.size} surfaces x 2 themes`);

const accentMax = Math.max(0, ...[...accents.values()].map((l) => l.length));
console.log(`accent: ${[...accents.values()].filter((l) => l.length === 1).length}`
  + ` of ${accents.size} surfaces name a primary action (most on one surface: ${accentMax})`);

const withHeader = [...headers].filter(([, g]) => g.count === 1);
console.log(`header: ${withHeader.length} of ${headers.size} routes wear the page header`
  + ` (title x=${[...new Set(withHeader.map(([, g]) => g.titleLeft))].join('/')},`
  + ` actions right=${[...new Set(withHeader.map(([, g]) => g.actionsRight).filter((x) => x !== null))].join('/')})`);

if (recorder) {
  console.log(`recorder: transcript ${recorder.transcript}px vs notes ${recorder.notes}px at rest`
    + ` (${recorder.notesFocused}px while writing), record edge ${recorder.edge?.height ?? 0}px`);
}

// Only the document routes are asked to lock a header; a pane route scrolls nothing itself.
const documentPages = [...locks].filter(([, m]) => !m.bleed);

console.log(
  `saved transcript: ${turns?.timed ?? 0}/${turns?.count ?? 0} turn(s) timed\n`
  + `calendar rows: ${rows.get('route:calendar')?.length ?? 0}, widest `
  + `${Math.max(0, ...(rows.get('route:calendar') ?? []).map((r) => r.width))}px\n`
  + `history table: ${tables.get('route:history')?.rows ?? 0} row(s), `
  + `${tables.get('route:history')?.when.length ?? 0} mono timestamp(s)\n`
  + `copy control: ${copyControls.size} tab(s), named `
  + `${[...new Set([...copyControls.values()].map((m) => `"${m.label}"`))].join(' / ') || 'nothing'}\n`
  + `record card at ${SHORT.width}x${SHORT.height}: ends ${fold?.bottom ?? '?'}px, Start ${fold?.start ?? '?'}px\n`
  + `glass: chrome measured on ${glassSeen} surface(s) — blurring where the page passes behind`
  + ` it, tinted where it does not, off under Increase Contrast and Reduce Transparency\n`
  + `pickers: ${pickersSeen} list(s) of values opened\n`
  + `controls: ${measuredSteppers} stepper(s) measured; `
  + `form columns: ${[...fieldWidths].map(([n, w]) => `${n} ${[...new Set(w)].join('/')}px`).join(', ') || 'none'}`,
);

const errors = [];

// A row that opens something on click owns the pixel a pointer aims at first.
if (stolenRowCentres === null) {
  errors.push('the library surface was never measured for row hit targets');
} else if (stolenRowCentres.length) {
  errors.push(
    `${stolenRowCentres.length} list row(s) do not own their own centre:\n`
    + stolenRowCentres.map((t) => `  ${t}`).join('\n'),
  );
}

// Library row titles fit the column's default width.
if (clippedTitles === null) {
  errors.push('the library surface was never measured for clipped titles (M8)');
} else if (clippedTitles.length) {
  errors.push(
    `${clippedTitles.length} library title(s) clipped at the column's default width (M8):\n`
    + clippedTitles.map((t) => `  ${t}`).join('\n'),
  );
}

// A control that steps a value is one control.
// Prose lines stay within the measure.
if (longLines.length) {
  errors.push(`prose runs past its measure (D2):\n${[...new Set(longLines)].map((t) => `  ${t}`).join('\n')}`);
}

// The record screen at the window's minimum size: card and button inside it.
if (!fold) {
  errors.push('the record card was never measured against the shortest window (D10)');
} else if (fold.start === null) {
  errors.push('the record card paints no start button (D10)');
} else if (fold.bottom > fold.window || fold.start > fold.window) {
  errors.push(
    `the record card runs off a ${SHORT.width}x${SHORT.height} window (D10): card ends at `
    + `${fold.bottom}px, Start at ${fold.start}px`,
  );
}

// The copy control keeps its name and edge across the tabs.
if (copyControls.size === 0) {
  errors.push('no tab was measured for its copy control (D4)');
} else {
  const labels = new Set([...copyControls.values()].map((m) => m.label));
  if (labels.size > 1) {
    errors.push(`the copy control changes its name between tabs (D4): ${[...labels].join(', ')}`);
  }
  const drift = [...copyControls].filter(([, m]) =>
    Math.abs(m.right - m.actionRight) > 120 || Math.abs(m.top - m.actionTop) > 4);
  if (drift.length) {
    errors.push(`the copy control sits away from the action group (D4):\n${drift
      .map(([n, m]) => `  ${n}: copy ends ${m.right}px at y=${m.top}, Action ends ${m.actionRight}px at y=${m.actionTop}`)
      .join('\n')}`);
  }
}

// The history table: mono timestamps, no underlined titles.
const history = tables.get('route:history');
if (!history) {
  errors.push('the history table was never measured (D7)');
} else {
  if (history.when.length === 0) {
    errors.push('the history table paints no timestamp column (D7)');
  }
  const plain = history.when.filter((w) => !w.mono).length;
  if (plain) errors.push(`${plain} history timestamp(s) render outside the mono face (D7)`);
  const ragged = history.when.filter((w) => !w.nums.includes('tabular-nums')).length;
  if (ragged) errors.push(`${ragged} history timestamp(s) do not line up on the digit (D7)`);
  if (history.underlined) {
    errors.push(`${history.underlined} link(s) in the history table are underlined at rest (D7)`);
  }
}

// Calendar rows: inside the measure, control beside the text.
const calRows = rows.get('route:calendar');
if (!calRows) {
  errors.push('no calendar row was measured (D8)');
} else {
  const wide = calRows.filter((r) => r.width > MEASURE_MAX);
  if (wide.length) {
    errors.push(`${wide.length} calendar row(s) run past the page measure (D8): `
      + `${wide.map((r) => `${r.width}px`).join(', ')} against ${MEASURE_MAX}px`);
  }
  const far = calRows.filter((r) => r.gap != null && r.gap > 420);
  if (far.length) {
    errors.push(`${far.length} calendar row(s) put Dismiss out of reach of the title (D8): `
      + `${far.map((r) => `${r.gap}px`).join(', ')}`);
  }
}

// Every saved turn carries its time.
if (!turns) {
  errors.push('the saved transcript was never measured for its timestamps (D6)');
} else if (turns.count === 0) {
  errors.push('the transcript tab paints no attributed turn (D6)');
} else if (turns.timed < turns.count) {
  errors.push(`${turns.count - turns.timed} of ${turns.count} saved turn(s) carry no time (D6)`);
}

// Glass is chrome, and each setting turns it off.
if (glassSeen === 0) errors.push('no surface painted the chrome as glass (M2)');
if (glass.length) {
  errors.push(`glass outside the chrome, or chrome without it (M2):\n${[...new Set(glass)]
    .map((t) => `  ${t}`).join('\n')}`);
}
if (glassOff.length) {
  errors.push(`the material does not turn off (M2):\n${glassOff.map((t) => `  ${t}`).join('\n')}`);
}

// Every list of values is the app's own, on the app's own surface.
if (pickersSeen === 0) {
  errors.push('no list-of-values control was measured (D15)');
}
if (pickers.length) {
  errors.push(`pickers that do not open the app's own list (D15):\n${[...new Set(pickers)]
    .map((t) => `  ${t}`).join('\n')}`);
}

// Nested corners are concentric.
if (corners.length) {
  errors.push(`corners inside corners are not concentric (D17):\n${[...new Set(corners)]
    .map((t) => `  ${t}`).join('\n')}`);
}

if (steppers.length) errors.push(`detached stepper parts (D18):\n${steppers.map((t) => `  ${t}`).join('\n')}`);

// A form column's fields measure the same.
const raggedFields = [...fieldWidths]
  .filter(([, w]) => new Set(w).size > 1)
  .map(([n, w]) => `  ${n}: ${[...new Set(w)].sort((a, b) => a - b).join('px, ')}px`);
if (raggedFields.length) {
  errors.push(`form fields in one column disagree on width (D19):\n${raggedFields.join('\n')}`);
}

// One filled accent per surface.
// The recorder is the one route without a page header.
const HEADERLESS_ROUTES = new Set(['route:record']);
const headerRoutes = [...headers].filter(([n]) => !HEADERLESS_ROUTES.has(n));
const noHeader = headerRoutes.filter(([, g]) => g.count !== 1);
if (noHeader.length) {
  errors.push(
    `${noHeader.length} route(s) do not paint exactly one .page-header (M6):\n`
    + noHeader.map(([n, g]) => `  ${n}: ${g.count} found`).join('\n'),
  );
}
const aligned = headerRoutes.filter(([, g]) => g.count === 1);
const titleXs = new Set(aligned.map(([, g]) => g.titleLeft));
const actionXs = new Set(aligned.filter(([, g]) => g.actionsRight !== null).map(([, g]) => g.actionsRight));
if (titleXs.size > 1 || actionXs.size > 1) {
  errors.push(
    'page headers disagree on where the title starts or the actions end (M6):\n'
    + aligned.map(([n, g]) => `  ${n}: title x=${g.titleLeft}, actions right=${g.actionsRight ?? '—'}`).join('\n'),
  );
}

// The header locks and the page runs under it.
if (documentPages.length === 0) {
  errors.push('no route was measured for a locked header (M6)');
}
let locked = 0;
for (const [name, m] of locks) {
  const before = errors.length;
  if (m.missing) {
    errors.push(`${name} paints a page with no ${m.missing} (M6)`);
  } else if (m.shellScrolls) {
    errors.push(`${name} sits in a shell that scrolls too — the page is the one`
      + ' scroller, so .main__body must not take the wheel (M6)');
  } else if (m.bleed) {
    if (!m.header) {
      errors.push(`${name} paints a page with no header (M6)`);
    } else if (m.overflow !== 'hidden' || m.scrolled !== 0) {
      errors.push(`${name} scrolls its whole page (overflow-y: ${m.overflow},`
        + ` moved ${m.scrolled}px) — a pane route scrolls inside its panes (M6)`);
    }
  } else if (m.overflow !== 'auto' && m.overflow !== 'scroll') {
    errors.push(`${name} does not take the wheel: the page is overflow-y: ${m.overflow} (M6)`);
  } else if (m.scrolled !== STEP) {
    errors.push(`${name} is not the scroller: it moved ${m.scrolled}px of ${STEP}px (M6)`);
  } else if (m.headerMoved > 1) {
    errors.push(`${name} carries its header off the top — it moved ${m.headerMoved}px`
      + ` while the page scrolled ${m.scrolled}px (M6)`);
  } else if (Math.abs(m.bodyMoved - m.scrolled) > 1) {
    errors.push(`${name} does not run its body under the header: the body moved`
      + ` ${m.bodyMoved}px while the page scrolled ${m.scrolled}px (M6)`);
  } else if (m.covered > 1) {
    errors.push(`${name} scrolls a row to ${m.covered}px under the header band —`
      + ' the band has to leave room for what is scrolled into view (M6)');
  }
  if (errors.length === before && !m.bleed) locked++;
}
console.log(`header lock: ${locked} of ${documentPages.length} document route(s) run their page`
  + ' under a pinned header, notices pinned with them, the band steady across the scrollbar gutter');
if (sectionReset.length) {
  errors.push(`a Settings section does not open at its top (M6):\n${sectionReset
    .map((t) => `  ${t}`).join('\n')}`);
}
if (navPin.length) {
  errors.push(`Settings' section list does not pin against the band (M6):\n${navPin
    .map((t) => `  ${t}`).join('\n')}`);
}
if (noticePin.length) {
  errors.push(`a page notice does not hold its place under the band (M6):\n${noticePin
    .map((t) => `  ${t}`).join('\n')}`);
}
if (gutter.length) {
  errors.push(`the header band moves with the scrollbar gutter (M6):\n${gutter
    .map((t) => `  ${t}`).join('\n')}`);
}

const crowded = [...accents].filter(([, list]) => list.length > 1);
if (crowded.length) {
  errors.push(
    `${crowded.length} surface(s) paint more than one filled accent — the accent`
    + ` marks one action per screen (M3):\n`
    + crowded.map(([name, list]) =>
      `  ${name}  (${list.length})\n` + list.map((b) => `      ${b}`).join('\n')).join('\n'),
  );
}

// While recording, the live transcript is larger than the notes box, and the recording edge is painted.
if (recorder === null) {
  errors.push('the live recorder was never measured for balance (M9)');
} else {
  if (recorder.transcript === null || recorder.notes === null) {
    errors.push('the live recorder paints no transcript or no notes surface (M9)');
  } else if (recorder.transcript < recorder.notes) {
    errors.push(`the notes box outranks the live transcript at rest (M9):`
      + ` notes ${recorder.notes}px, transcript ${recorder.transcript}px`);
  }
  if (!(recorder.notesFocused > recorder.notes)) {
    errors.push('the notes box does not grow while it is written in (M9):'
      + ` ${recorder.notes}px at rest, ${recorder.notesFocused}px focused`);
  }
  const paint = recorder.edge?.paint ?? '';
  if (!recorder.edge || recorder.edge.height < 2 || /rgba\(0, 0, 0, 0\)|transparent/.test(paint)) {
    errors.push('a live recording paints no edge on the window — the state has to be'
      + ` visible without reading (M9): ${recorder.edge ? `${recorder.edge.height}px ${paint}` : 'no .rec-edge'}`);
  }
}

// A container holding nothing names what belongs there and offers the step that fills it.
for (const name of EXPECTED_SURFACES.filter((n) => n.startsWith('empty:'))) {
  const e = empties.get(name);
  if (!e) {
    errors.push(`${name} was never measured for its empty state (D5)`);
  } else if (e.count !== 1) {
    errors.push(`${name} paints ${e.count} empty-state block(s) — it has to paint one (D5)`);
  } else if (!e.title || e.body.length < 40) {
    errors.push(`${name} names nothing while it is empty (D5): title "${e.title}",`
      + ` ${e.body.length} characters of body`);
  } else if (e.step === null) {
    errors.push(`${name} says it is empty and offers no step that fills it (D5)`);
  }
}
const emptySummary = [...empties].map(([n, e]) => `${n.slice(6)} → ${e.step}`).join(', ');
if (emptySummary) console.log(`empty states: ${emptySummary}`);

const missing = EXPECTED_SURFACES.filter((s) => !swept.has(s));
const extra = [...swept].filter((s) => !EXPECTED_SURFACES.includes(s)).sort();
if (missing.length) {
  errors.push(`${missing.length} surface(s) the sweep never reached — unmeasured, not passing:\n`
    + missing.map((s) => `  ${s}`).join('\n'));
}
if (extra.length) {
  errors.push(`${extra.length} surface(s) not in EXPECTED_SURFACES — add them:\n`
    + extra.map((s) => `  ${s}`).join('\n'));
}
if (misses.length) {
  errors.push(`${misses.length} command(s) the fixture world cannot answer — teach`
    + ` e2e/fixtures/world.ts, or the screen behind them renders empty:\n`
    + misses.map((c) => `  ${c}`).join('\n'));
}
if (failures.length) {
  errors.push(`${failures.length} unreadable:\n${failures.join('\n')}`);
}

if (errors.length) {
  console.error(`\n${errors.join('\n\n')}`);
  process.exit(1);
}
console.log('all text meets WCAG AA against its actual background');
