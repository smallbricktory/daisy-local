// Real-engine layout check for the live-transcript chat bubbles.
//
// Runs the actual workspace.css against fixture markup in Playwright's
// WebKit AND Chromium and asserts bubble geometry. WebKitGTK (Linux) and
// WKWebView (macOS) are the engines the shipped app renders with; jsdom
// tests can't see engine-specific intrinsic-sizing behavior — a WebKit
// flex bug collapsed short bubbles to one character per line twice before
// this check existed.
//
// Usage: node test/webkit-layout.mjs   (from apps/frontend; browsers via
// `npx playwright install webkit chromium`)

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { webkit, chromium } from 'playwright';

const styles = ['reset.css', 'workspace.css']
  .map((f) => readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../src/styles', f), 'utf8'))
  .join('\n');

// Mirrors ActiveSession.tsx's live transcript markup exactly.
const html = `<style>${styles}</style>
<div id="controls" style="display:flex;gap:8px;align-items:center;width:600px">
  <input type="text" placeholder="text box" style="width:120px">
  <select><option>pick list</option></select>
  <input type="date">
  <button class="btn">Button</button>
</div>
<input id="libsearch" class="lib-search__input" placeholder="search">
<h1 class="h1" id="h1probe">Heading <em>accent</em></h1>
<span class="wm-name brand-wm-probe" id="wmprobe" style="font-family:'Cormorant Garamond', Georgia, serif">Daisy</span>
<div class="transcript-tab" style="width:600px">
  <div class="turn them" id="tt-them"><span class="who">Jane</span><span class="turn-text">Hello there.</span></div>
  <div class="turn me" id="tt-me"><span class="turn-text">Hi Jane.</span></div>
  <div class="turn" id="tt-plain"><span class="turn-text">## Chunk 2</span></div>
</div>
<div class="notebook__transcript" style="width:600px">
  <div class="turn them" id="short-them"><span class="who">Them</span><span class="turn-text">Shh.</span></div>
  <div class="turn me" id="short-me"><span class="turn-text">Yeah.</span></div>
  <div class="turn them" id="long"><span class="who">Them</span><span class="turn-text">Yeah, I mean, I think this is a much longer sentence that definitely needs to wrap at the maximum bubble width instead of stretching on forever and ever and ever.</span></div>
  <div class="turn turn--pause" id="pause">[paused at 12:01]</div>
</div>`;

const failures = [];
const check = (engine, name, ok, detail) => {
  if (!ok) failures.push(`${engine}: ${name} — ${detail}`);
};

for (const [name, engine] of [['webkit', webkit], ['chromium', chromium]]) {
  const browser = await engine.launch();
  const page = await browser.newPage();
  await page.setContent(html);
  const m = await page.evaluate(() => {
    const container = document.querySelector('.notebook__transcript');
    const cs = getComputedStyle(container);
    const raw = container.getBoundingClientRect();
    // content box: bubbles align inside the container's padding
    const box = {
      left: raw.left + parseFloat(cs.paddingLeft),
      right: raw.right - parseFloat(cs.paddingRight),
      width: raw.width - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight),
    };
    const grab = (id) => {
      const turn = document.getElementById(id);
      const text = turn.querySelector('.turn-text');
      const r = turn.getBoundingClientRect();
      const t = text ? text.getBoundingClientRect() : null;
      const lineHeight = text ? parseFloat(getComputedStyle(text).lineHeight) : 0;
      return {
        width: r.width,
        left: r.left - box.left,
        right: box.right - r.right,
        lines: t ? Math.round(t.height / lineHeight) : null,
      };
    };
    return {
      container: box.width,
      shortThem: grab('short-them'),
      shortMe: grab('short-me'),
      long: grab('long'),
      pause: grab('pause'),
      whoFont: getComputedStyle(document.querySelector('.who')).fontFamily,
      themWhoWeight: getComputedStyle(document.querySelector('#short-them .who')).fontWeight,
      meAlign: getComputedStyle(document.querySelector('#short-me .turn-text')).textAlign,
      meHasWho: !!document.querySelector('#short-me .who'),
      controlHeights: [...document.querySelectorAll('#controls input, #controls select, #controls button')]
        .map((el) => Math.round(el.getBoundingClientRect().height)),
      libSearchH: Math.round(document.getElementById('libsearch').getBoundingClientRect().height),
      ttThemW: Math.round(document.getElementById('tt-them').getBoundingClientRect().width),
      ttMeAlign: getComputedStyle(document.querySelector('#tt-me .turn-text')).textAlign,
      ttPlainW: Math.round(document.getElementById('tt-plain').getBoundingClientRect().width),
      h1Font: getComputedStyle(document.getElementById('h1probe')).fontFamily,
      h1EmStyle: getComputedStyle(document.querySelector('#h1probe em')).fontStyle,
    };
  });
  await browser.close();

  // Bubbles are a constant reading width — never sized by the ASR chunk.
  // THEM left, ME right, 60% each: the middle fifth overlaps.
  const expected = m.container * 0.6;
  check(name, 'short THEM single line', m.shortThem.lines === 1, `lines=${m.shortThem.lines}`);
  check(name, 'short THEM fixed width', Math.abs(m.shortThem.width - expected) < 20, `width=${m.shortThem.width} vs ${expected}`);
  check(name, 'short THEM hugs left', m.shortThem.left < 2, `left=${m.shortThem.left}`);
  check(name, 'short ME single line', m.shortMe.lines === 1, `lines=${m.shortMe.lines}`);
  check(name, 'short ME hugs right', m.shortMe.right < 2, `right=${m.shortMe.right}`);
  check(name, 'widths uniform', Math.abs(m.shortThem.width - m.long.width) < 2, `${m.shortThem.width} vs ${m.long.width}`);
  check(name, 'THEM label bold', Number(m.themWhoWeight) >= 700, `weight=${m.themWhoWeight}`);
  check(name, 'ME has no label', !m.meHasWho, 'found .who in me bubble');
  check(name, 'ME text right-justified', m.meAlign === 'right', `textAlign=${m.meAlign}`);
  // The saved transcript shares the chat layout; unattributed rows stay full width.
  check(name, 'saved THEM 60%-width', Math.abs(m.ttThemW - 360) < 20, `w=${m.ttThemW}`);
  check(name, 'saved ME right-justified', m.ttMeAlign === 'right', `textAlign=${m.ttMeAlign}`);
  check(name, 'saved plain row full width', m.ttPlainW > 540, `w=${m.ttPlainW}`);
  // Headings are display-sans; display italics aren't shipped so em stays upright.
  check(name, 'h1 uses Archivo', m.h1Font.includes('Archivo'), `font=${m.h1Font}`);
  check(name, 'h1 em not italic', m.h1EmStyle === 'normal', `fontStyle=${m.h1EmStyle}`);
  // Long turns wrap inside the fixed measure instead of one word per line.
  check(name, 'long turn wraps', m.long.lines >= 2 && m.long.lines <= 12, `lines=${m.long.lines}`);
  // Pause markers are plain full-width rows.
  check(name, 'pause row full width', m.pause.width > m.container * 0.9, `width=${m.pause.width}`);
  // --font-mono resolves to a real stack (a circular var() definition
  // computes to the engine default and shipped unnoticed once).
  check(name, 'who label resolves --font-mono', m.whoFont.includes('JetBrains Mono'), `fontFamily=${m.whoFont}`);
  // One shared control height: text box, select, date, button all --control-h.
  check(name, 'controls share --control-h', m.controlHeights.every((h) => h === 38), `heights=${m.controlHeights}`);
  check(name, 'lib-search special stays compact', m.libSearchH === 30, `h=${m.libSearchH}`);
  console.log(`${name}: bubble geometry ok (them=${Math.round(m.shortThem.width)}px/1ln, me right-aligned, long=${m.long.lines}ln)`);
}

if (failures.length) {
  console.error('\nFAILURES:');
  for (const f of failures) console.error('  ' + f);
  process.exit(1);
}
console.log('webkit-layout: all checks passed');
