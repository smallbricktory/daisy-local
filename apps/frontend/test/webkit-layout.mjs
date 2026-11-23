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
  <div class="select"><button type="button" class="select__trigger"><span class="select__value">pick list</span><span class="select__caret">&#9662;</span></button></div>
  <input type="date">
  <button class="btn">Button</button>
</div>
<h1 class="h1" id="h1probe">Heading <em>accent</em></h1>
<span class="wm-name brand-wm-probe" id="wmprobe" style="font-family:'Cormorant Garamond', Georgia, serif">Daisy</span>
<div id="stage" style="position:relative;width:900px;height:300px;background:var(--bg);overflow:hidden">
  <div style="position:absolute;inset:0;padding:12px;font-size:13px" id="stage-text">
    Behind the rail sits the page it tints.
  </div>
  <div class="rail" id="frame-rail" style="position:absolute;left:0;top:0;width:220px;height:300px">
    <div class="nav-item nav-item--record">
      <span class="nav-item--record__dot nav-item--record__dot--live"></span>
      Recording
    </div>
  </div>
</div>
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
      monoFont: (() => {
        const probe = document.createElement('span');
        probe.style.fontFamily = 'var(--font-mono)';
        document.body.appendChild(probe);
        const f = getComputedStyle(probe).fontFamily;
        probe.remove();
        return f;
      })(),
      themWhoWeight: getComputedStyle(document.querySelector('#short-them .who')).fontWeight,
      themWhoDisplay: getComputedStyle(document.querySelector('#short-them .who')).display,
      themWhoBlock: (() => {
        const who = document.querySelector('#short-them .who').getBoundingClientRect();
        const text = document.querySelector('#short-them .turn-text').getBoundingClientRect();
        return text.top >= who.bottom - 1;
      })(),
      meAlign: getComputedStyle(document.querySelector('#short-me .turn-text')).textAlign,
      meHasWho: !!document.querySelector('#short-me .who'),
      controlH: parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--control-h')),
      controlHeights: [...document.querySelectorAll('#controls input, #controls .select__trigger, #controls > button')]
        .map((el) => Math.round(el.getBoundingClientRect().height)),
      ttThemW: Math.round(document.getElementById('tt-them').getBoundingClientRect().width),
      ttMeAlign: getComputedStyle(document.querySelector('#tt-me .turn-text')).textAlign,
      ttPlainW: Math.round(document.getElementById('tt-plain').getBoundingClientRect().width),
      h1Font: getComputedStyle(document.getElementById('h1probe')).fontFamily,
      h1EmStyle: getComputedStyle(document.querySelector('#h1probe em')).fontStyle,
    };
  });
  // M2: the material sits behind the one thing that animates while recording —
  // the rail's pulse. A blurred layer re-filtered every frame is how the
  // recording path grows a per-frame paint, so both engines are timed with the
  // glass on and with it off, and the two have to stay in the same league.
  const frames = async (reduced) => page.evaluate(async (off) => {
    const root = document.documentElement;
    if (off) root.dataset.reduceTransparency = '1'; else delete root.dataset.reduceTransparency;
    // Warm up, then time 90 frames.
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    const deltas = [];
    let last = performance.now();
    await new Promise((done) => {
      const tick = () => {
        const now = performance.now();
        deltas.push(now - last);
        last = now;
        if (deltas.length >= 90) done(); else requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    });
    deltas.sort((a, b) => a - b);
    return { median: deltas[45], p90: deltas[81] };
  }, reduced);
  const glassOn = await frames(false);
  const glassOff = await frames(true);

  await browser.close();

  // Bubbles are a constant reading width — never sized by the ASR chunk.
  // D6: a bubble is the size of what was said. THEM anchors left, ME right,
  // and neither passes the 60% cap.
  const cap = m.container * 0.6;
  check(name, 'short THEM single line', m.shortThem.lines === 1, `lines=${m.shortThem.lines}`);
  check(name, 'short THEM hugs its words', m.shortThem.width < cap * 0.5, `width=${m.shortThem.width} vs cap ${cap}`);
  check(name, 'short THEM hugs left', m.shortThem.left < 2, `left=${m.shortThem.left}`);
  check(name, 'short ME single line', m.shortMe.lines === 1, `lines=${m.shortMe.lines}`);
  check(name, 'short ME hugs right', m.shortMe.right < 2, `right=${m.shortMe.right}`);
  check(name, 'short ME hugs its words', m.shortMe.width < cap * 0.5, `width=${m.shortMe.width} vs cap ${cap}`);
  check(name, 'long turn stops at the cap', Math.abs(m.long.width - cap) < 20, `width=${m.long.width} vs cap ${cap}`);
  check(name, 'a long turn is wider than a short one', m.long.width > m.shortThem.width + 40, `${m.long.width} vs ${m.shortThem.width}`);
  check(name, 'the speaker name sits above the words', m.themWhoBlock, `display=${m.themWhoDisplay}`);
  check(name, 'THEM label bold', Number(m.themWhoWeight) >= 700, `weight=${m.themWhoWeight}`);
  check(name, 'ME has no label', !m.meHasWho, 'found .who in me bubble');
  check(name, 'ME text right-justified', m.meAlign === 'right', `textAlign=${m.meAlign}`);
  // The saved transcript shares the chat layout; unattributed rows stay full width.
  check(name, 'saved THEM hugs its words', m.ttThemW < 360 * 0.6, `w=${m.ttThemW}`);
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
  check(name, '--font-mono resolves to a real stack', m.monoFont.includes('JetBrains Mono'), `fontFamily=${m.monoFont}`);
  // One shared control height: text box, select, date, button all --control-h,
  // read off the page so the ladder stays the only place the number lives.
  check(name, 'controls share --control-h', m.controlHeights.every((h) => h === m.controlH),
    `heights=${m.controlHeights} vs --control-h ${m.controlH}`);
  // 33ms is the 30fps floor; the pulse is opacity-only and should sit far
  // under it on both engines, glass or no glass.
  check(name, 'the pulse keeps its frames behind glass', glassOn.p90 < 33,
    `p90=${glassOn.p90.toFixed(1)}ms (glass off: ${glassOff.p90.toFixed(1)}ms)`);
  check(name, 'glass costs the pulse no frame time', glassOn.median < glassOff.median + 8,
    `median glass ${glassOn.median.toFixed(1)}ms vs flat ${glassOff.median.toFixed(1)}ms`);
  console.log(`${name}: bubble geometry ok (them=${Math.round(m.shortThem.width)}px/1ln, me right-aligned, long=${m.long.lines}ln)`);
  console.log(`${name}: pulse frames median ${glassOn.median.toFixed(1)}ms glass / `
    + `${glassOff.median.toFixed(1)}ms flat, p90 ${glassOn.p90.toFixed(1)}ms / ${glassOff.p90.toFixed(1)}ms`);
}

if (failures.length) {
  console.error('\nFAILURES:');
  for (const f of failures) console.error('  ' + f);
  process.exit(1);
}
console.log('webkit-layout: all checks passed');
