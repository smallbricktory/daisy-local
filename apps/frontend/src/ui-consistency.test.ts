// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Static UI-consistency lint over the component sources. Fails on elements
// that bypass the design-system idioms (workspace.css tokens/classes).
import { describe, it, expect } from 'vitest';
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { walk } from './test/walk';
import { tagAttrs } from './test/jsx-scan';

const isTsx = (n: string) => n.endsWith('.tsx') && !n.includes('.test.');

// Root-level views (App.tsx) are linted alongside routes/ and components/.
const ROOT_TSX = readdirSync('src', { withFileTypes: true })
  .filter((e) => e.isFile() && isTsx(e.name))
  .map((e) => `src/${e.name}`);
const FILES = [...ROOT_TSX, ...walk('src/routes', isTsx), ...walk('src/components', isTsx)];

function read(f: string): string {
  // Line comments stripped so prose mentioning `<button>` is not scanned.
  return readFileSync(f, 'utf8').replace(/^\s*\/\/.*$/gm, '');
}

function lineOf(s: string, idx: number): number {
  return s.slice(0, idx).split('\n').length;
}

/** A stylesheet with its comments blanked; line numbers are kept. */
function readCss(f: string): string {
  return readFileSync(f, 'utf8').replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, ' '));
}

/** Every selector in a rule head. */
function selectorsOf(head: string): string[] {
  return head.split(',').map((x) => x.trim()).filter(Boolean);
}

/** The token tables: where colour and material are written down. */
const TOKEN_TABLE = /^:root(\[data-theme="(?:dark|light)"\])?$/;

/** The theme a rule only applies in, when it is scoped to one. */
function themeOf(selector: string): 'light' | 'dark' | null {
  const m = /^:root\[data-theme="(dark|light)"\]/.exec(selector);
  return m ? (m[1] as 'light' | 'dark') : null;
}

// What the rendered sweep and these rules both read.
const SURFACES = JSON.parse(readFileSync('test/design-surfaces.json', 'utf8')) as {
  chrome: string[]; surfaces: string[];
};

// The stylesheets the design system owns. reset.css is the unthemed
// baseline and sits outside it.
const SHEETS = ['src/styles/workspace.css', 'src/mini-window.css', 'src/reminder-window.css'];

describe('UI consistency', () => {
  it('every <button> declares a className (btn/btn--sm/btn-link or a semantic class)', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      let idx = 0;
      for (;;) {
        const j = s.indexOf('<button', idx);
        if (j < 0) break;
        const attrs = tagAttrs(s, j + 7);
        if (!attrs.includes('className')) violations.push(`${f}:${lineOf(s, j)}`);
        idx = j + 7;
      }
    }
    expect(violations, `buttons without className:\n${violations.join('\n')}`).toEqual([]);
  });

  it('every heading names its tier with a class', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      if (f.endsWith('MarkdownView.tsx')) continue; // renders user markdown
      const s = read(f);
      for (const m of s.matchAll(/<h([1-6])\b/g)) {
        const attrs = tagAttrs(s, (m.index ?? 0) + m[0].length);
        if (!attrs.includes('className')) violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
      }
    }
    expect(violations, `headings without .h class:\n${violations.join('\n')}`).toEqual([]);
  });

  it('monospace comes from the --font-mono token, never a hand-written stack', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/JetBrains/g)) {
        violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
      }
    }
    expect(violations, `hand-written mono stacks (use var(--font-mono)):\n${violations.join('\n')}`).toEqual([]);
  });

  it('mono renders machine data and the eyebrow, nothing else', () => {
    // Selectors allowed the mono face: machine-written values and the eyebrow label.
    const DATA_OR_EYEBROW = new Set([
      '.eyebrow', '.summary-pane__eyebrow',
      '.turn .turn-ts', '.session-row__when', '.session-row__dur', '.search-hit__date',
      '.summary-pane__meta', '.summary-pane__sid', '.build-stamp',
      '.color-picker__hex', '.data-table__url', '.data-table__when', '.bgfin-detail',
      '.run-step__n, .run-step__ms',
      '.textarea--mono', '.summary-edit',
      // The utility a component reaches for when machine output sits inside a
      // sentence: a level in dBFS, a model id, a license id, a path.
      '.mono',
    ]);
    const violations: string[] = [];
    for (const f of SHEETS) {
      const s = read(f);
      for (const m of s.matchAll(/(^|\})([^{}]*)\{([^}]*font-family:\s*var\(--font-mono\)[^}]*)\}/gm)) {
        const sel = m[2].replace(/\/\*[\s\S]*?\*\//g, '').trim();
        if (sel === ':root' || DATA_OR_EYEBROW.has(sel)) continue;
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} ${sel}`);
      }
    }
    expect(violations, `mono set on something that is not machine data or an\neyebrow — use the UI face, or add the selector to DATA_OR_EYEBROW with the\nreason it carries machine output:\n${violations.join('\n')}`).toEqual([]);
  });

  it('no inline link-button styling (use the .btn-link class)', () => {
    const violations: string[] = [];
    const pat = /background:\s*'none'[^}]*textDecoration:\s*'underline'|textDecoration:\s*'underline'[^}]*background:\s*'none'/s;
    for (const f of FILES) {
      const s = read(f);
      const m = s.match(pat);
      if (m) violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
    }
    expect(violations, `inline link-button styles (use .btn-link):\n${violations.join('\n')}`).toEqual([]);
  });

  it('buttons with the same intent share the same styling', () => {
    // label pattern → required/forbidden class fragments
    // Intent governs EMPHASIS (primary/danger/neutral), never size —
    // size belongs to the row a button sits in (see the sibling-size rule).
    const INTENTS: Array<{ verb: RegExp; requires?: string[]; requiresAny?: string[]; exact?: string; forbids?: string[] }> = [
      { verb: /^Save$/, requires: ['btn--primary'] },
      { verb: /^Save as…?$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      { verb: /^Cancel$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      { verb: /^Edit$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      // A row's Delete lives in its overflow; an editor footer's is an outlined .btn--danger.
      { verb: /^Delete( \w+)?…?$/, requiresAny: ['btn--danger', 'menu__item--danger'] },
      { verb: /^New /, requires: ['btn'], forbids: ['btn--danger'] },
    ];
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      let idx = 0;
      for (;;) {
        const j = s.indexOf('<button', idx);
        if (j < 0) break;
        const attrs = tagAttrs(s, j + 7);
        const end = j + 7 + attrs.length + 1;
        // A self-closing <button /> has no body.
        const close = attrs.trimEnd().endsWith('/') ? end : s.indexOf('</button>', end);
        const body = s.slice(end, close < 0 ? end : close);
        // Label candidates: the plain text plus every quoted string inside
        // {…} children (conditional labels like {busy ? 'Saving…' : 'Save'}).
        const plain = body
          .replace(/\{[^}]*\}|<[^>]+>/g, ' ')
          .replace(/\s+/g, ' ')
          .trim();
        const quoted = [...body.matchAll(/\{[^}]*\}/g)]
          .flatMap((e) => [...e[0].matchAll(/'([^']+)'/g)].map((q) => q[1]));
        const labels = [plain, ...quoted].filter(Boolean);
        // Class candidates: the string literal, or every template/quoted
        // branch of a className={…} expression.
        const lit = attrs.match(/className="([^"]*)"/);
        const clsBranches = lit
          ? [lit[1]]
          : [...(attrs.match(/className=\{[^}]*\}/)?.[0] ?? '').matchAll(/['`]([^'`]+)['`]/g)].map((q) => q[1]);
        for (const rule of INTENTS) {
          const label = labels.find((l) => rule.verb.test(l));
          if (!label) continue;
          // Custom semantic controls stay exempt unless a btn-like class is present.
          const btnLike = clsBranches.some((c) => c.split(' ').includes('btn'));
          const semantic = clsBranches.length > 0 && clsBranches.every((c) => /__|--[a-z]/.test(c) && !c.split(' ').includes('btn'));
          if (!btnLike && semantic) continue;
          const ok = clsBranches.some((cls) =>
            (rule.exact ? cls === rule.exact : true)
            && (rule.requires ?? []).every((r) => cls.includes(r))
            && (!rule.requiresAny || rule.requiresAny.some((r) => cls.includes(r)))
            && !(rule.forbids ?? []).some((r) => cls.includes(r)));
          if (!ok) violations.push(`${f}:${lineOf(s, j)} "${label}" [${clsBranches.join(' | ') || '(none)'}] fails intent rule ${rule.verb}`);
        }
        idx = j + 7;
      }
    }
    expect(violations, `intent-inconsistent buttons:\n${violations.join('\n')}`).toEqual([]);
  });

  // Every colour-bearing property is checked: hex and rgba() literals alike. Shadow properties are excluded.
  it('colours come from tokens — no hardcoded colour in component styles', () => {
    const violations: string[] = [];
    const PROP = /(background|backgroundColor|color|border|borderColor|borderTop|borderBottom|borderLeft|borderRight|outline|fill|stroke)\s*:\s*('[^']*'|`[^`]*`)/g;
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(PROP)) {
        const value = m[2];
        // A hex inside var(--token, #fallback) is a fallback, not a literal;
        // the rule below proves the token exists.
        const bare = value.replace(/var\(--[\w-]+\s*,[^)]*\)/g, '');
        const hit = bare.match(/#[0-9A-Fa-f]{3,8}\b|\brgba?\(|\b(black|white)\b/i);
        if (hit) violations.push(`${f}:${lineOf(s, m.index ?? 0)} ${m[1]}: ${hit[0]}`);
      }
    }
    expect(violations, `hardcoded colours (use a semantic token):\n${violations.join('\n')}`).toEqual([]);
  });

  // The same rule for the stylesheets. Literals belong in the token blocks and
  // in the theme-preview swatches, which have to paint the theme the app is not
  // currently in.
  it('colours come from tokens — no hardcoded colour in the stylesheets', () => {
    const PROP = '(background|background-color|color|border|border-color|border-top|'
      + 'border-bottom|border-left|border-right|outline|outline-color|fill|stroke|'
      + 'caret-color|accent-color)';
    const violations: string[] = [];
    for (const f of SHEETS) {
      // Comments are blanked rather than removed so line numbers survive.
      const css = read(f).replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, ' '));
      for (const rule of css.matchAll(/(?<=^|\})([^{}]+)\{([^{}]*)\}/g)) {
        const selector = rule[1].trim();
        // Where colour is written down, as opposed to painted.
        if (selectorsOf(selector).every((x) => TOKEN_TABLE.test(x))) continue;
        // A swatch showing the light theme must be light while the app is dark.
        if (selector.includes('.theme-card__prev')) continue;
        for (const d of rule[2].matchAll(new RegExp(PROP + '\\s*:\\s*([^;]+)', 'g'))) {
          const bare = d[2].replace(/var\(--[\w-]+\s*,[^)]*\)/g, '');
          const hit = bare.match(/#[0-9A-Fa-f]{3,8}\b|\brgba?\(|\b(black|white)\b/i);
          if (hit) {
            const at = (rule.index ?? 0) + (rule[0].indexOf(d[0]));
            violations.push(`${f}:${lineOf(css, at)} ${d[1]}: ${hit[0]}`);
          }
        }
      }
    }
    expect(violations, `hardcoded colours in CSS (use a semantic token):\n${violations.join('\n')}`).toEqual([]);
  });

  // Spacing off the ladder is counted; the count may fall, never rise. Lower CEILING when it does.
  it('raw spacing does not increase', () => {
    const CEILING = 164;
    const seen: Record<string, number> = {};
    let count = 0;
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/\b(padding|margin|gap|row-gap|column-gap)(-top|-bottom|-left|-right)?:\s*([^;}]+)/g)) {
        for (const t of m[3].trim().split(/\s+/)) {
          if (!/^-?\d+px$/.test(t)) continue;
          count++;
          seen[t] = (seen[t] ?? 0) + 1;
        }
      }
    }
    const worst = Object.entries(seen).sort((a, b) => b[1] - a[1]).slice(0, 6)
      .map(([v, n]) => `  ${n}× ${v}`).join('\n');
    expect(
      count,
      count > CEILING
        ? `raw spacing rose to ${count} (ceiling ${CEILING}); most common:\n${worst}`
        : `raw spacing fell to ${count} — lower CEILING to ${count}`,
    ).toBe(CEILING);
  });

  // Leading comes from the ramp; a line-height number written at a call site is flagged.
  it('leading comes from a token — in the stylesheets and at every call site', () => {
    const violations: string[] = [];
    const ok = (v: string) => /^var\(--lh-(text|tight|flush)\)$/.test(v) || v === 'inherit';
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/(?<!-)\bline-height:\s*([^;}]+)/g)) {
        const v = m[1].trim();
        if (ok(v)) continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} line-height: ${v}`);
      }
    }
    // The same rule where a component writes it: `lineHeight` at a call site is
    // leading the ramp does not own.
    for (const f of FILES) {
      const src = read(f);
      for (const m of src.matchAll(/lineHeight:\s*([^,}\n]+)/g)) {
        const v = m[1].trim().replace(/,$/, '');
        if (ok(v.replace(/^'|'$/g, ''))) continue;
        violations.push(`${f}:${lineOf(src, m.index ?? 0)} lineHeight: ${v}`);
      }
    }
    expect(violations, `hand-written leading (use var(--lh-text|tight|flush)):\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // A role holding a literal colour is also declared in the dark block. Aliases (var(--x)) resolve per theme.
  it('every colour role holding a literal value is declared in both themes', () => {
    const css = readFileSync('src/styles/workspace.css', 'utf8')
      .replace(/\/\*[\s\S]*?\*\//g, '');
    const block = (selector: string) => {
      const at = css.indexOf(selector + ' {');
      if (at < 0) throw new Error(`no ${selector} block in workspace.css`);
      const body = css.slice(at, css.indexOf('}', at));
      const out = new Map<string, string>();
      for (const m of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out.set(m[1], m[2].trim());
      return out;
    };
    const light = block(':root');
    const dark = block(':root[data-theme="dark"]');
    // A value is theme-owned when a colour literal survives with every
    // var() reference removed: color-mix(in srgb, var(--gold) 45%, …) takes
    // its colour from a role that already flips.
    const literal = (v: string) =>
      /#[0-9A-Fa-f]{3,8}\b|\brgba?\(/.test(v.replace(/var\(--[\w-]+\)/g, ''));
    const missing = [...light]
      .filter(([, v]) => literal(v))
      .map(([k]) => k)
      .filter((k) => !dark.has(k));
    expect(missing, `colour roles the dark theme inherits from light:\n${missing.join('\n')}`)
      .toEqual([]);
  });

  // Every var() names a declared custom property.
  it('every var(--token) referenced by a component is declared', () => {
    const declared = new Set<string>();
    for (const f of ['src/styles/workspace.css', 'src/styles/reset.css']) {
      for (const m of readFileSync(f, 'utf8').matchAll(/(--[\w-]+)\s*:/g)) declared.add(m[1]);
    }
    // Per-element properties a component sets inline (--tag-color on a chip,
    // --turn-tint on a turn) are declared where they are set, not in :root.
    for (const f of FILES) {
      for (const m of read(f).matchAll(/['"`](--[\w-]+)['"`]\s*:/g)) declared.add(m[1]);
    }
    const violations: string[] = [];
    // The stylesheets and the gallery are checked for undeclared tokens too.
    for (const f of [...FILES, ...SHEETS, 'test/gallery.html']) {
      const s = read(f);
      for (const m of s.matchAll(/var\((--[\w-]+)/g)) {
        if (!declared.has(m[1])) violations.push(`${f}:${lineOf(s, m.index ?? 0)} ${m[1]}`);
      }
    }
    expect(violations, `undeclared custom properties (always fall back, never theme):\n${violations.join('\n')}`).toEqual([]);
  });

  // Every declared token is referenced somewhere.
  it('every token declared in :root is referenced', () => {
    const root = readFileSync('src/styles/workspace.css', 'utf8').match(/^:root \{([\s\S]*?)^\}/m)?.[1] ?? '';
    const referenced = new Set<string>();
    for (const f of [...FILES, ...SHEETS, 'test/gallery.html']) {
      for (const m of read(f).matchAll(/var\((--[\w-]+)/g)) referenced.add(m[1]);
    }
    const unused = [...root.matchAll(/^\s+(--[\w-]+)\s*:/gm)]
      .map((m) => m[1])
      .filter((t) => !referenced.has(t));
    expect(unused, `declared and never called:\n${unused.join('\n')}`).toEqual([]);
  });

  // A switch is a setting that applies the moment it is flipped; a checkbox is
  // one of several values a form collects and a Save or Continue button
  // commits. The tell is in the handler: a checkbox that writes on change —
  // a tauri call, a persisted update — is a switch wearing the wrong control.
  it('a checkbox that writes on change is a switch', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/type="checkbox"/g)) {
        // The handler runs to the end of the JSX attribute list.
        const tail = s.slice(m.index ?? 0, (m.index ?? 0) + 600);
        const handler = tail.match(/onChange=\{([\s\S]*?)\n\s*(\/>|[a-zA-Z-]+=)/)?.[1] ?? '';
        if (/\btauri\.|\bvoid\s/.test(handler)) {
          violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
        }
      }
    }
    expect(violations, `checkboxes that write on change — use <Toggle>:\n${violations.join('\n')}`).toEqual([]);
  });

  // One icon family: the dingbats (✓ ✕ ✦ ✎ ✂ ↻) are glyphs in the UI font. Colour emoji are flagged.
  it('components draw with glyphs, not colour emoji', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/[\u{1F000}-\u{1FAFF}\u{FE0F}]/gu)) {
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} ${m[0]}`);
      }
    }
    expect(violations, `colour emoji — use a dingbat that takes currentColor:\n${violations.join('\n')}`).toEqual([]);
  });

  it('form controls keep the shared height — no inline padding/height/fontSize', () => {
    const EXEMPT_TYPES = /type="(checkbox|radio|range|file|color|hidden)"/;
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      let idx = 0;
      for (;;) {
        const j = s.indexOf('<input', idx);
        if (j < 0) break;
        const attrs = tagAttrs(s, j + 6);
        idx = j + 6;
        if (EXEMPT_TYPES.test(attrs)) continue;
        const ident = attrs.match(/style=\{([A-Za-z_$][\w$]*)\}/);
        if (ident) {
          violations.push(`${f}:${lineOf(s, j)} opaque style={${ident[1]}} on input (inline the layout props; sizing via workspace.css)`);
          continue;
        }
        const style = attrs.match(/style=\{\{([^]*?)\}\}/);
        if (!style) continue;
        if (/\.\.\./.test(style[1])) {
          violations.push(`${f}:${lineOf(s, j)} spread style on input (inline the layout props; sizing via workspace.css)`);
          continue;
        }
        const bad = style[1].match(/\b(padding|paddingTop|paddingBottom|height|fontSize|font)\s*:/);
        if (bad && !/height:\s*'auto'/.test(style[1]))
          violations.push(`${f}:${lineOf(s, j)} inline ${bad[1]} on input (size via workspace.css --control-h)`);
      }
    }
    expect(violations, `inline-sized form controls:\n${violations.join('\n')}`).toEqual([]);
  });

  // Glass is chrome only: the rail, the page-header band, menus and the toast stack.
  // Any other rule using backdrop-filter is flagged.
  // These rules check the material stays on chrome and every chrome surface takes the token.
  it('glass is chrome only, and every glass token has its way out', () => {
    const CHROME = new Set(SURFACES.chrome);
    const violations: string[] = [];
    const tinted = new Set<string>();
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/([^{}]+)\{([^}]*backdrop-filter:[^}]*)\}/g)) {
        // Turning the material off is every selector's right; carrying it is
        // chrome's alone.
        if (/backdrop-filter:\s*none/.test(m[2])) continue;
        for (const sel of selectorsOf(m[1])) {
          if (!CHROME.has(sel)) {
            violations.push(`${f}:${lineOf(css, m.index ?? 0)} backdrop-filter on ${sel}`);
            continue;
          }
          if (!/background:\s*var\(--glass-/.test(m[2])) {
            violations.push(`${f}:${lineOf(css, m.index ?? 0)} ${sel} blurs without a --glass-* background`);
          }
        }
      }
      for (const m of css.matchAll(/([^{}]+)\{([^}]*background:\s*var\(--glass-[^}]*)\}/g)) {
        for (const sel of selectorsOf(m[1])) if (CHROME.has(sel)) tinted.add(sel);
      }
    }
    for (const sel of CHROME) {
      if (!tinted.has(sel)) violations.push(`${sel} is chrome with no --glass-* background`);
    }
    // Three settings turn the material off, and each has to answer for every token. The app's own setting reaches CSS as an attribute.
    const sheet = readFileSync('src/styles/workspace.css', 'utf8');
    const tokens = [...sheet.matchAll(/(--glass-[a-z]+):/g)].map((m) => m[1]);
    const OUTS: [string, string][] = [
      ['Increase Contrast', '@media (prefers-contrast: more)'],
      ['reduced transparency', '@media (prefers-reduced-transparency: reduce)'],
      ['the app attribute (macOS Reduce Transparency, Linux)', ':root[data-reduce-transparency="1"]'],
    ];
    for (const [what, open] of OUTS) {
      const at = sheet.indexOf(open);
      if (at < 0) {
        violations.push(`workspace.css has no ${what} block (${open})`);
        continue;
      }
      const body = sheet.slice(at, sheet.indexOf('\n}', sheet.indexOf('{', at)));
      for (const t of new Set(tokens)) {
        if (!body.includes(`${t}:`)) violations.push(`${what} leaves ${t} on`);
      }
      if (!/--glass-blur:\s*none/.test(body)) violations.push(`${what} leaves the blur on`);
    }
    expect(violations, `glass outside the chrome, or with no way out:\n${violations.join('\n')}`).toEqual([]);
  });

  // A native <select> is not used; the app draws its own list in components/Select.tsx.
  it('a list of values is the app\'s own Select', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/<select\b/g)) {
        violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
      }
    }
    expect(violations, `native <select> — use <Select> from components/Select.tsx:\n${violations.join('\n')}`).toEqual([]);
  });

  // A <label> does not wrap the picker trigger.
  it('a picker is named with htmlFor, never by a label wrapped around it', async () => {
    const ts = (await import('typescript')).default;
    const violations: string[] = [];
    for (const f of FILES) {
      const src = readFileSync(f, 'utf8');
      const sf = ts.createSourceFile(f, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: import('typescript').Node) => {
        if (ts.isJsxElement(node) && node.openingElement.tagName.getText() === 'label'
            && /<Select\b/.test(node.getText())) {
          violations.push(`${f}:${sf.getLineAndCharacterOfPosition(node.getStart()).line + 1}`);
        }
        ts.forEachChild(node, visit);
      };
      visit(sf);
    }
    expect(
      violations,
      'a <Select> inside a <label> — the label forwards an option\'s click back to\n'
      + 'the trigger and the list re-opens. Bind the label with htmlFor and give the\n'
      + `picker that id:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  it('one button size — btn--sm is retired', () => {
    const violations: string[] = [];
    for (const f of [...FILES, ...SHEETS]) {
      const s = f.endsWith('.css') ? readCss(f) : read(f);
      for (const m of s.matchAll(/btn--sm/g)) {
        violations.push(`${f}:${lineOf(s, m.index ?? 0)}`);
      }
    }
    const css = readFileSync('src/styles/workspace.css', 'utf8');
    if (/\.btn--sm\s*\{/.test(css)) violations.push('workspace.css defines .btn--sm');
    expect(violations, `btn--sm sightings (all buttons are --control-h; .btn--big is the hero exception):\n${violations.join('\n')}`).toEqual([]);
  });

  it('a flex row carries at most one primary/record button (danger may coexist)', async () => {
    const ts = (await import('typescript')).default;
    const violations: string[] = [];
    for (const f of FILES) {
      const src = readFileSync(f, 'utf8');
      const sf = ts.createSourceFile(f, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      const visit = (node: import('typescript').Node) => {
        if (ts.isJsxElement(node) && /display:\s*'flex'/.test(node.openingElement.attributes.getText())
            && !/flexDirection:\s*'column'/.test(node.openingElement.attributes.getText())) {
          let emphasized = 0;
          for (const c of node.children) {
            let el: import('typescript').Node | undefined =
              ts.isJsxElement(c) || ts.isJsxSelfClosingElement(c) ? c : undefined;
            if (!el && ts.isJsxExpression(c) && c.expression) {
              let e: import('typescript').Node = c.expression;
              while (ts.isBinaryExpression(e)) e = e.right;
              if (ts.isParenthesizedExpression(e)) e = e.expression;
              if (ts.isJsxElement(e) || ts.isJsxSelfClosingElement(e)) el = e;
            }
            if (!el) continue;
            const opening = ts.isJsxElement(el) ? el.openingElement : (el as import('typescript').JsxSelfClosingElement);
            if (opening.tagName.getText() !== 'button') continue;
            const attrs = opening.attributes.getText();
            if (/btn--(primary|record)/.test(attrs)) emphasized++;
          }
          if (emphasized > 1) {
            const { line } = sf.getLineAndCharacterOfPosition(node.getStart());
            violations.push(`${f}:${line + 1} has ${emphasized} primary/record buttons in one flex row`);
          }
        }
        node.forEachChild(visit);
      };
      visit(sf);
    }
    expect(violations, `rows with competing primary buttons:\n${violations.join('\n')}`).toEqual([]);
  });

  it('a button that shows a busy label is disabled while busy', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      let idx = 0;
      for (;;) {
        const j = s.indexOf('<button', idx);
        if (j < 0) break;
        const attrs = tagAttrs(s, j + 7);
        const end = j + 7 + attrs.length + 1;
        // A self-closing <button /> has no body.
        const close = attrs.trimEnd().endsWith('/') ? end : s.indexOf('</button>', end);
        const body = s.slice(end, close < 0 ? end : close);
        idx = j + 7;
        // Busy-label pattern: a ternary rendering "Something…" while working.
        if (!/\?\s*'[A-Z][a-zA-Z]+ing…'/.test(body) && !/\?\s*'[A-Z][a-zA-Z]+ing…'/.test(attrs)) continue;
        if (!/\bdisabled=/.test(attrs)) {
          violations.push(`${f}:${lineOf(s, j)} busy-label button without disabled=`);
        }
      }
    }
    expect(violations, `busy buttons that stay clickable:\n${violations.join('\n')}`).toEqual([]);
  });

  it('typography comes from tokens — no literal font stacks outside the tiers', () => {
    const violations: string[] = [];
    // TSX: inline fontFamily must be a token or inherit.
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/fontFamily:\s*(['"`])((?:(?!\1).)+)\1/g)) {
        const v = m[2];
        if (v === 'inherit' || /^var\(--font-(mono|display|brand)\)$/.test(v)) continue;
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} fontFamily "${v}" (use var(--font-mono)/var(--font-display)/inherit)`);
      }
    }
    // font-family declarations are tokens or the whitelisted wordmark serif.
    for (const f of SHEETS) {
      const css = readFileSync(f, 'utf8');
      for (const m of css.matchAll(/(?<!-)\bfont-family:\s*([^;]+);/g)) {
        const v = m[1].trim();
        if (v.startsWith('var(--font-')) continue;
        if (/^"(JetBrains Mono|Bricolage Grotesque)"/.test(v) && lineOf(css, m.index ?? 0) < 30) continue; // :root token defs
        if (v === 'inherit') continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} font-family ${v}`);
      }
    }
    expect(violations, `off-token typography:\n${violations.join('\n')}`).toEqual([]);
  });

  // The other direction: every custom property a call site sets is read by a stylesheet rule.
  it('every custom property a component sets inline is read by a rule', () => {
    const read_by = new Set<string>();
    for (const f of [...SHEETS, 'src/styles/reset.css']) {
      for (const m of readFileSync(f, 'utf8').matchAll(/var\((--[\w-]+)/g)) read_by.add(m[1]);
    }
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      const set = [
        ...s.matchAll(/['"`](--[\w-]+)['"`]\s*:/g),
        ...s.matchAll(/setProperty\(\s*['"`](--[\w-]+)['"`]/g),
      ];
      for (const m of set) {
        // A component may also read its own property back in the same style
        // object (`width: 'calc(var(--x) * 1px)'`).
        if (read_by.has(m[1]) || s.includes(`var(${m[1]}`)) continue;
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} ${m[1]}`);
      }
    }
    expect(
      violations,
      `custom property set inline that no rule reads — the element paints its\n` +
      `default instead of the value:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  // Appearance belongs in the stylesheet; layout set inline is fine. A custom
  // property is how a call site hands the stylesheet a value it alone knows —
  // a tag's colour, a meter's level — so `--x` passes and `color` does not.
  it('no component paints itself inline', () => {
    const APPEARANCE = new Set([
      'color', 'background', 'backgroundColor', 'border', 'borderTop', 'borderBottom',
      'borderLeft', 'borderRight', 'borderColor', 'borderRadius', 'boxShadow',
      'fontFamily', 'fontWeight', 'textTransform', 'letterSpacing', 'opacity', 'outline',
      'fill', 'stroke',
    ]);
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/style=\{\{([\s\S]*?)\}\}/g)) {
        for (const p of m[1].matchAll(/([a-zA-Z]+)\s*:/g)) {
          if (!APPEARANCE.has(p[1])) continue;
          violations.push(`${f}:${lineOf(s, (m.index ?? 0) + (p.index ?? 0))} ${p[1]}`);
        }
      }
    }
    expect(
      violations,
      `appearance set inline — put it in a class, or hand the stylesheet a\n` +
      `custom property (style={{ '--tag-color': hex }}) if only the call site\n` +
      `knows the value:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  // alert()/confirm() must be the app's own imported versions.
  it('alert/confirm calls import the app dialogs', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      const calls = [...s.matchAll(/(?<![.\w])(alert|confirm)\s*\(/g)];
      if (calls.length === 0) continue;
      if (/from '[^']*lib\/confirm'/.test(s)) continue;
      violations.push(`${f}:${lineOf(s, calls[0].index ?? 0)} (${calls[0][1]})`);
    }
    expect(
      violations,
      `native alert/confirm — import them from lib/confirm:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  it('CSS custom properties never reference themselves (circular var = silently invalid)', () => {
    const violations: string[] = [];
    for (const f of ['src/styles/workspace.css', 'src/styles/reset.css']) {
      const s = readFileSync(f, 'utf8');
      // Declarations only: preceded by { ; or newline, value free of braces
      // (a selector like `.btn--danger:hover { … }` must not match).
      for (const m of s.matchAll(/[{;\n]\s*(--[\w-]+)\s*:\s*([^;{}]+);/g)) {
        if (m[2].includes(`var(${m[1]})`)) {
          violations.push(`${f}:${lineOf(s, m.index ?? 0)} (${m[1]})`);
        }
      }
    }
    expect(violations, `self-referential custom properties:\n${violations.join('\n')}`).toEqual([]);
  });

  // ===== Contrast, read off the token table =====
  //
  // The token table is read statically for every text/ground pair, in both themes.

  /** The two theme blocks of workspace.css, aliases resolved to literals. */
  function themePalettes(): { light: Map<string, string>; dark: Map<string, string> } {
    const css = readFileSync('src/styles/workspace.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
    const block = (selector: string) => {
      const at = css.indexOf(selector + ' {');
      if (at < 0) throw new Error(`no ${selector} block in workspace.css`);
      const out = new Map<string, string>();
      for (const m of css.slice(at, css.indexOf('}', at)).matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
        out.set(m[1], m[2].trim());
      }
      return out;
    };
    const light = block(':root');
    // Dark declares only what it changes; everything else it inherits.
    const dark = new Map(light);
    for (const [k, v] of block(':root[data-theme="dark"]')) dark.set(k, v);
    const resolve = (t: Map<string, string>) => {
      const out = new Map<string, string>();
      for (const [k] of t) {
        let v = t.get(k)!;
        for (let i = 0; i < 4 && /^var\(--[\w-]+\)$/.test(v); i++) v = t.get(v.slice(4, -1)) ?? v;
        out.set(k, v);
      }
      return out;
    };
    return { light: resolve(light), dark: resolve(dark) };
  }

  /** An opaque colour as sRGB, or null when it is not one the rule can judge
   *  (color-mix, a translucent rgba, a gradient — the browser sweep has those). */
  function rgb(value: string): [number, number, number] | null {
    const v = value.trim();
    let m = /^#([0-9a-f]{6})$/i.exec(v);
    if (m) return [0, 2, 4].map((i) => parseInt(m![1].slice(i, i + 2), 16)) as [number, number, number];
    m = /^#([0-9a-f]{3})$/i.exec(v);
    if (m) return [...m[1]].map((c) => parseInt(c + c, 16)) as [number, number, number];
    m = /^rgb\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)\s*\)$/i.exec(v);
    if (m) return [+m[1], +m[2], +m[3]];
    return null;
  }

  function ratio(fg: [number, number, number], bg: [number, number, number]): number {
    const lum = (c: [number, number, number]) => {
      const f = (x: number) => (x / 255 <= 0.03928 ? x / 255 / 12.92 : ((x / 255 + 0.055) / 1.055) ** 2.4);
      return 0.2126 * f(c[0]) + 0.7152 * f(c[1]) + 0.0722 * f(c[2]);
    };
    const [a, b] = [lum(fg), lum(bg)].sort((x, y) => y - x);
    return (a + 0.05) / (b + 0.05);
  }

  // The grounds a text colour lands on when its own rule names no background.
  const GROUNDS = ['--bg', '--surface', '--raised', '--overlay'];

  const SIZE: Record<string, number> = {
    '--t-meta': 11, '--t-small': 12, '--t-ui': 13, '--t-read': 16, '--t-title': 20, '--t-display': 26,
  };

  /** Text the app paints, with the ground it sits on: the rule's own
   *  background when it sets one, every page ground when it does not. */
  type Pair = {
    where: string; fg: string; bg: string | null; size: number; bold: boolean;
    /** Set when the rule only applies in one theme. */
    theme?: 'light' | 'dark';
  };

  function textPairs(): Pair[] {
    const pairs: Pair[] = [];
    const add = (
      where: string, fg: string, bg: string | null, size: number, bold: boolean,
      theme?: 'light' | 'dark',
    ) => pairs.push({ where, fg, bg, size, bold, ...(theme ? { theme } : {}) });

    // --- the stylesheets, rule by rule ---
    for (const file of SHEETS) {
      const css = readCss(file);
      for (const m of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        const heads = selectorsOf(m[1]).filter((x) => !x.startsWith('@'));
        const body = m[2];
        const fg = /(?:^|[;\s])color:\s*var\((--[\w-]+)\)/.exec(body)?.[1];
        if (!fg) continue;
        const bg = /(?:^|[;\s])background(?:-color)?:\s*var\((--[\w-]+)\)\s*[;}]?/.exec(body)?.[1] ?? null;
        // A background this rule sets to something unresolvable (a gradient, a
        // color-mix) is the browser sweep's job, not this one.
        if (!bg && /(?:^|[;\s])background(?:-color)?:/.test(body)) continue;
        const sizeToken = /font-size:\s*var\((--t-[\w-]+)\)/.exec(body)?.[1];
        const sizePx = /font-size:\s*(\d+(?:\.\d+)?)px/.exec(body)?.[1];
        const size = sizeToken ? SIZE[sizeToken] ?? 13 : sizePx ? +sizePx : 13;
        const weight = /font-weight:\s*(\d+|bold)/.exec(body)?.[1];
        for (const selector of heads) {
          // A token table declares colour, it does not paint text.
          if (TOKEN_TABLE.test(selector)) continue;
          add(
            `${file}:${lineOf(css, m.index ?? 0)} ${selector}`, fg, bg, size,
            weight === 'bold' || +(weight ?? 0) >= 700, themeOf(selector) ?? undefined,
          );
        }
      }
    }

    // --- inline style objects, one object at a time ---
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/style=\{\{([\s\S]*?)\}\}/g)) {
        const body = m[1];
        const fg = /(?:^|[,{\s])color:\s*'var\((--[\w-]+)\)'/.exec(body)?.[1];
        if (!fg) continue;
        const bg = /(?:^|[,{\s])(?:background|backgroundColor):\s*'var\((--[\w-]+)\)'/.exec(body)?.[1] ?? null;
        if (!bg && /(?:^|[,{\s])(?:background|backgroundColor):/.test(body)) continue;
        const sizeToken = /fontSize:\s*'var\((--t-[\w-]+)\)'/.exec(body)?.[1];
        const sizePx = /fontSize:\s*(\d+)/.exec(body)?.[1];
        const size = sizeToken ? SIZE[sizeToken] ?? 13 : sizePx ? +sizePx : 13;
        const weight = /fontWeight:\s*'?(\d+|bold)'?/.exec(body)?.[1];
        add(`${f}:${lineOf(s, m.index ?? 0)}`, fg, bg, size, weight === 'bold' || +(weight ?? 0) >= 700);
      }
    }

    return pairs;
  }

  /** Every pair judged against a floor, in both themes. */
  function judgePairs(
    tables: { light: Map<string, string>; dark: Map<string, string> },
    floor: (p: Pair) => number,
  ): string[] {
    const out: string[] = [];
    for (const p of textPairs()) {
      const need = floor(p);
      for (const theme of (p.theme ? [p.theme] : ['light', 'dark']) as ('light' | 'dark')[]) {
        const table = tables[theme];
        const f = rgb(table.get(p.fg) ?? p.fg);
        if (!f) break;
        for (const g of p.bg ? [p.bg] : GROUNDS) {
          const b = rgb(table.get(g) ?? g);
          if (!b) continue;
          const r = ratio(f, b);
          if (r >= need) continue;
          out.push(`${p.where}  ${theme} ${r.toFixed(2)}:1 (needs ${need})  ${p.fg} on ${g}`);
        }
      }
    }
    return out;
  }

  // The AA floor is 4.5. Large text (>=24px, or >=18.66px bold) is allowed
  // 3.0, which is why a rule setting --t-display is read for its size.
  it('a token used as a text colour clears WCAG AA in both themes', () => {
    const violations = judgePairs(themePalettes(), (p) =>
      p.size >= 24 || (p.size >= 18.66 && p.bold) ? 3.0 : 4.5);
    expect(violations, `text below WCAG AA on the ground it sits on:\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // ===== Radius, motion, elevation =====
  //

  // Corners come from --r-1..--r-4, --r-full or --bubble-radius. A circle is
  // 50% and a square corner is 0 — both are shapes, not steps on the ladder.
  it('corners come from the radius ladder', () => {
    const STEP = /^(var\(--(r-[\w-]+|bubble-radius)\)|50%|0|inherit)$/;
    /** A shorthand is up to four corners; a calc() deriving a nested corner
     *  from the ladder is one value with spaces inside it. */
    const OK = (value: string) => {
      const v = value.trim();
      if (/^calc\(.*var\(--r-[\w-]+\).*\)$/.test(v)) return true;
      let depth = 0, part = '', parts: string[] = [];
      for (const c of v) {
        if (c === '(') depth++;
        if (c === ')') depth--;
        if (/\s/.test(c) && depth === 0) { if (part) parts.push(part); part = ''; continue; }
        part += c;
      }
      if (part) parts.push(part);
      return parts.length > 0 && parts.every((t) => STEP.test(t));
    };
    const violations: string[] = [];
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/(?<!-)\bborder-radius:\s*([^;}]+)/g)) {
        const v = m[1].trim();
        if (OK(v)) continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} border-radius: ${v}`);
      }
    }
    for (const f of FILES) {
      const s2 = read(f);
      for (const m of s2.matchAll(/borderRadius:\s*([^,}\n]+)/g)) {
        const v = m[1].trim().replace(/,$/, '').replace(/^'|'$/g, '');
        if (OK(v)) continue;
        violations.push(`${f}:${lineOf(s2, m.index ?? 0)} borderRadius: ${v}`);
      }
    }
    expect(violations, `corners off the ladder (use var(--r-1..--r-4|--r-full)):\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // Every transition names a duration token.
  it('transitions name a duration token', () => {
    const violations: string[] = [];
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/(?<!-)\btransition(?:-duration)?:\s*([^;}]+)/g)) {
        const v = m[1].trim();
        if (v === 'none' || /var\(--dur-[\w-]+\)/.test(v)) continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} transition: ${v}`);
      }
    }
    for (const f of FILES) {
      const s2 = read(f);
      for (const m of s2.matchAll(/transition:\s*'([^']+)'/g)) {
        if (/var\(--dur-[\w-]+\)/.test(m[1])) continue;
        violations.push(`${f}:${lineOf(s2, m.index ?? 0)} transition: ${m[1]}`);
      }
    }
    expect(violations, `transitions that reduced motion cannot stop:\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // A keyframe loop runs at its own tempo, so it cannot be stopped by zeroing
  // a token — it has to be named.
  it('reduced motion reaches every animation', () => {
    const uncovered: string[] = [];
    // Every sheet loads into the same document, so one reduced-motion block
    // can quiet a selector another sheet animates — and does.
    const quieted = new Set<string>();
    const sheets = SHEETS.map((f) => [f, readCss(f)] as const);
    for (const [, css] of sheets) {
      const at = css.indexOf('@media (prefers-reduced-motion: reduce)');
      if (at < 0) continue;
      const block = css.slice(at, css.indexOf('\n}', at));
      for (const m of block.matchAll(/([^{}]+)\{[^{}]*animation(?:-name)?:\s*none/g)) {
        for (const sel of selectorsOf(m[1])) quieted.add(sel);
      }
    }
    for (const [f, css] of sheets) {
      for (const m of css.matchAll(/([^{}@]+)\{([^{}]*)\}/g)) {
        const body = m[2];
        if (!/(?:^|[;\s])animation(?:-name)?:\s*(?!none)/.test(body)) continue;
        for (const sel of selectorsOf(m[1])) {
          if (quieted.has(sel)) continue;
          uncovered.push(`${f}:${lineOf(css, m.index ?? 0)} ${sel}`);
        }
      }
    }
    expect(uncovered, `animations no reduced-motion rule stops:\n${uncovered.join('\n')}`).toEqual([]);
  });

  // Elevation is four tiers and two shadows. An outer shadow that is neither
  // is a fifth tier nobody named. An inset shadow is a drawn edge, a different
  // thing — the halo and the tag chip's ring are inset by design.
  it('outer shadows come from the elevation tokens', () => {
    const OK = /^(var\(--(shadow-raised|shadow-overlay|halo|glow)\)|none)$/;
    const violations: string[] = [];
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/(?<!-)\bbox-shadow:\s*([^;}]+)/g)) {
        const v = m[1].trim();
        if (v.startsWith('inset') || OK.test(v)) continue;
        if (/^:root/.test(css.slice(0, m.index ?? 0).split('{').slice(-2)[0]?.trim() ?? '')) continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} box-shadow: ${v}`);
      }
    }
    for (const f of FILES) {
      const s2 = read(f);
      for (const m of s2.matchAll(/boxShadow:\s*([^,}\n]+)/g)) {
        const v = m[1].trim().replace(/,$/, '').replace(/^'|'$/g, '');
        if (v.startsWith('inset') || OK.test(v)) continue;
        violations.push(`${f}:${lineOf(s2, m.index ?? 0)} boxShadow: ${v}`);
      }
    }
    expect(violations, `shadows outside the elevation tokens:\n${violations.join('\n')}`).toEqual([]);
  });

  // ===== Increase Contrast =====
  //
  // Roles that carry text below 7:1 are answered under prefers-contrast: more.

  /** The palettes as `prefers-contrast: more` leaves them. */
  function contrastPalettes(): { light: Map<string, string>; dark: Map<string, string> } {
    const css = readFileSync('src/styles/workspace.css', 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
    const base = themePalettes();
    const at = css.indexOf('@media (prefers-contrast: more)');
    expect(at, 'no prefers-contrast block in workspace.css').toBeGreaterThan(-1);
    const block = css.slice(at, css.indexOf('\n}\n', at));
    const over = (selector: string) => {
      const i = block.indexOf(selector + ' {');
      const out = new Map<string, string>();
      if (i < 0) return out;
      for (const m of block.slice(i, block.indexOf('}', i)).matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
        out.set(m[1], m[2].trim());
      }
      return out;
    };
    const light = new Map(base.light);
    for (const [k, v] of over(':root')) light.set(k, v);
    const dark = new Map(base.dark);
    // The dark block inside the media query inherits that query's light half.
    for (const [k, v] of over(':root')) if (!base.dark.has(k) || base.dark.get(k) === base.light.get(k)) dark.set(k, v);
    for (const [k, v] of over(':root[data-theme="dark"]')) dark.set(k, v);
    return { light, dark };
  }

  it('Increase Contrast lifts every text role to 7:1, and lowers none', () => {
    const base = themePalettes();
    const more = contrastPalettes();
    // The AAA floor: 7:1, and 4.5 for large text — Apple's "especially small
    // text", said in the numbers WCAG gives it.
    const floor = (p: Pair) => (p.size >= 24 || (p.size >= 18.66 && p.bold) ? 4.5 : 7.0);
    const short = judgePairs(more, floor);
    expect(short, `text roles Increase Contrast leaves under its floor:\n${short.join('\n')}`)
      .toEqual([]);
    // And it may never trade one role's legibility for another's.
    const lowered: string[] = [];
    for (const p of textPairs()) {
      for (const theme of ['light', 'dark'] as const) {
        const f0 = rgb(base[theme].get(p.fg) ?? p.fg);
        const f1 = rgb(more[theme].get(p.fg) ?? p.fg);
        if (!f0 || !f1) break;
        for (const g of p.bg ? [p.bg] : GROUNDS) {
          const b0 = rgb(base[theme].get(g) ?? g);
          const b1 = rgb(more[theme].get(g) ?? g);
          if (!b0 || !b1) continue;
          const was = ratio(f0, b0), now = ratio(f1, b1);
          if (now + 1e-9 < was) lowered.push(`${p.where} ${theme} ${p.fg} on ${g}: ${was.toFixed(2)} -> ${now.toFixed(2)}`);
        }
      }
    }
    expect(lowered, `Increase Contrast made these worse:\n${lowered.join('\n')}`).toEqual([]);
  });

  // Hairlines are boundaries, and Apple's Increase Contrast is where a reader
  // says they cannot see them. 3:1 is the non-text floor (WCAG 2.1 1.4.11).
  it('Increase Contrast makes hairlines visible', () => {
    const more = contrastPalettes();
    const violations: string[] = [];
    for (const theme of ['light', 'dark'] as const) {
      const f = rgb(more[theme].get('--line') ?? '');
      if (!f) continue;
      for (const g of GROUNDS) {
        const b = rgb(more[theme].get(g) ?? '');
        if (!b) continue;
        const r = ratio(f, b);
        if (r < 3.0) violations.push(`${theme} --line on ${g}: ${r.toFixed(2)} (needs 3.0)`);
      }
    }
    expect(violations, `hairlines still invisible under Increase Contrast:\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // An off switch is a control with no label of its own state: its track is
  // the only thing that says a control is there. 3:1 is the non-text floor
  // (WCAG 2.1 1.4.11).
  it('an off switch track reads as a control on every dark ground', () => {
    const dark = themePalettes().dark;
    const track = rgb(dark.get('--switch-off') ?? '');
    expect(track, '--switch-off is not an opaque colour in the dark theme').not.toBeNull();
    const violations: string[] = [];
    for (const g of GROUNDS) {
      const b = rgb(dark.get(g) ?? '');
      if (!b) continue;
      const r = ratio(track!, b);
      if (r < 3.0) violations.push(`dark --switch-off on ${g}: ${r.toFixed(2)} (needs 3.0)`);
    }
    expect(violations, `off switch track too faint:\n${violations.join('\n')}`).toEqual([]);
  });

  // ===== Stacking =====
  //
  // z-index values come from the named ladder.
  it('stacking comes from the ladder — no raw z-index', () => {
    const violations: string[] = [];
    for (const f of SHEETS) {
      const css = readCss(f);
      for (const m of css.matchAll(/(?<!-)\bz-index:\s*([^;}]+)/g)) {
        const v = m[1].trim();
        if (/^var\(--z-[\w-]+\)$/.test(v) || v === 'auto') continue;
        violations.push(`${f}:${lineOf(css, m.index ?? 0)} z-index: ${v}`);
      }
    }
    for (const f of FILES) {
      const s2 = read(f);
      for (const m of s2.matchAll(/zIndex:\s*([^,}\n]+)/g)) {
        const v = m[1].trim().replace(/,$/, '');
        if (/^'var\(--z-[\w-]+\)'$/.test(v) || v === "'auto'") continue;
        violations.push(`${f}:${lineOf(s2, m.index ?? 0)} zIndex: ${v}`);
      }
    }
    expect(violations, `raw stacking values (use var(--z-*)):\n${violations.join('\n')}`).toEqual([]);
  });

  // ===== Focus =====
  //
  // An indicator exists wherever the default one is removed, and the token that draws it (--ring) is visible against its ground.

  /** The colour a shorthand like `2px solid var(--x)` paints. */
  function outlineColour(value: string): string | null {
    return /var\((--[\w-]+)\)/.exec(value)?.[1] ?? null;
  }

  // WCAG 2.2 §1.4.11: a focus indicator is a non-text contrast, and owes 3:1
  it('the focus ring can be seen on every ground, in both themes', () => {
    const { light, dark } = themePalettes();
    const token = outlineColour(light.get('--ring') ?? '');
    expect(token, '--ring paints no var() colour').not.toBeNull();
    const violations: string[] = [];
    for (const [theme, table] of [['light', light], ['dark', dark]] as const) {
      const f = rgb(table.get(token!) ?? '');
      expect(f, `--ring resolves to ${table.get(token!)} in ${theme}, which this rule cannot judge`)
        .not.toBeNull();
      for (const g of GROUNDS) {
        const b = rgb(table.get(g) ?? '');
        if (!b) continue;
        const r = ratio(f!, b);
        if (r < 3.0) violations.push(`${theme} ${r.toFixed(2)}:1 (needs 3.0)  ${token} on ${g}`);
      }
    }
    expect(violations, `the focus ring is invisible somewhere:\n${violations.join('\n')}`).toEqual([]);
  });

  // Removing the UA outline is fine; removing it and leaving nothing is not.
  // The ring may live on a different selector of the same component (the
  // editor kills it on the content div and draws it on the frame), so the
  // component — the selector's first class — is what has to own one.
  it('nothing removes the focus outline without drawing the ring', () => {
    const violations: string[] = [];
    for (const f of SHEETS) {
      const css = readCss(f);
      const blocks = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)]
        .flatMap((m) => selectorsOf(m[1]).map((sel) => ({ sel, body: m[2], at: m.index ?? 0 })));
      // Every component that draws the ring somewhere, by first class.
      const ringed = new Set(
        blocks.filter((b) => /outline:\s*var\(--ring\)/.test(b.body))
          .map((b) => /\.[\w-]+/.exec(b.sel)?.[0] ?? b.sel),
      );
      for (const b of blocks) {
        if (!/(?:^|[;\s])outline:\s*(none|0)\s*[;}]?/.test(b.body)) continue;
        const component = /\.[\w-]+/.exec(b.sel)?.[0] ?? b.sel;
        if (ringed.has(component)) continue;
        violations.push(`${f}:${lineOf(css, b.at)} ${b.sel}`);
      }
    }
    // A component may not reach out of the stylesheets to kill it either.
    for (const f of FILES) {
      const s2 = read(f);
      for (const m of s2.matchAll(/outline:\s*'(none|0)'/g)) {
        violations.push(`${f}:${lineOf(s2, m.index ?? 0)} inline outline: ${m[1]}`);
      }
    }
    expect(violations, `focus removed with nothing drawn in its place:\n${violations.join('\n')}`)
      .toEqual([]);
  });

  // ===== The harnesses that check the design system =====

  // A gate that reads a file outside the repo runs on one machine.
  it('the design-system harnesses read only tracked, repo-local files', async () => {
    const { execFileSync } = await import('node:child_process');
    const { dirname, join, normalize } = await import('node:path');
    // What a fresh clone has. A fixture the harness reads that is not in here
    // lives on one machine.
    const tracked = new Set(
      execFileSync('git', ['ls-files'], { encoding: 'utf8' }).split('\n').filter(Boolean),
    );
    const harnesses = [
      ...walk('test', (n) => n.endsWith('.mjs') || n.endsWith('.html')),
      ...walk('src/test', (n) => n.endsWith('.ts')),
    ];
    const ESCAPES = /\.\.\/\.\.\/\.\.\/|^\/(?:Users|home|root|var|tmp|etc)\/|^~\/|^[A-Za-z]:\\/;
    const violations: string[] = [];
    for (const f of harnesses) {
      const s2 = readFileSync(f, 'utf8');
      for (const m of s2.matchAll(/['"`]([^'"`\n${}]+)['"`]/g)) {
        const ref = m[1];
        const where = `${f}:${lineOf(s2, m.index ?? 0)}`;
        if (/^(?:https?:|data:|node:|@)/.test(ref) || ref.includes('node_modules')) continue;
        if (ESCAPES.test(ref)) {
          violations.push(`${where} reaches outside apps/frontend: ${ref}`);
          continue;
        }
        // Only a literal that names a file actually sitting there can be
        // judged; the rest are fragments a variable completes at run time.
        if (!/[\w-]\.(?:mjs|js|ts|tsx|json|html|css|wav|png|svg)$/.test(ref)) continue;
        const candidates = [
          normalize(join(dirname(f), ref)),
          normalize(ref.replace(/^\//, '')),
        ];
        for (const c of candidates) {
          if (!existsSync(c) || tracked.has(c)) continue;
          violations.push(`${where} reads an untracked file: ${c}`);
        }
      }
      if (/os\.homedir\(\)|process\.env\.(?:HOME|USERPROFILE)/.test(s2)) {
        violations.push(`${f} reads a path out of the home directory`);
      }
    }
    expect(violations, `harness fixtures a fresh clone does not have:\n${violations.join('\n')}`).toEqual([]);
  });

  // The sweep's surfaces are checked against the nav rail's items (EXPECTED_SURFACES) and the mini and reminder windows chosen by webview label.
  // Destructive actions live in a menu the reader opened, or inside a dialog opened for that one record, and they confirm. A row does not carry Delete at rest.
  it('a destructive action is demoted — a menu item, a dialog, or a confirm', async () => {
    const ts = (await import('typescript')).default;
    // "Clear" is left out on purpose: it empties a filter or a field as often
    // as it destroys data ("Clear dates", "Clear label").
    const VERB = /^(Delete|Discard|Remove|Unpair|Erase|Revoke)\b/;
    const violations: string[] = [];
    for (const f of FILES) {
      const src = readFileSync(f, 'utf8');
      const sf = ts.createSourceFile(f, src, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
      // `row` is the nearest enclosing JSX element — the button's actual
      // container, past any {cond && (…)} wrapper.
      const visit = (
        node: import('typescript').Node,
        inDialog: boolean,
        row: import('typescript').JsxElement | undefined,
      ) => {
        let dialog = inDialog;
        let nextRow = row;
        if (ts.isJsxElement(node) || ts.isJsxSelfClosingElement(node)) {
          const opening = ts.isJsxElement(node) ? node.openingElement : node;
          const attrs = opening.attributes.getText();
          if (/className=(?:"|'|\{')[^"'`]*\bmodal\b/.test(attrs)) dialog = true;
          if (opening.tagName.getText() === 'button') {
            const text = ts.isJsxElement(node)
              ? node.children.map((c) => c.getText()).join(' ').trim()
              : '';
            // An editor footer — the row that also carries this record's
            // primary Save — is where a stated Delete belongs. The Save has to
            // be a sibling: a section with a Save at the bottom would otherwise
            // exempt every Delete anywhere inside it.
            const footer = !!row && row.children.some((c) => {
              // {cond && (<button …/>)} is still a child of the row.
              let e: import('typescript').Node = c;
              if (ts.isJsxExpression(e) && e.expression) {
                e = e.expression;
                while (ts.isBinaryExpression(e)) e = e.right;
                if (ts.isParenthesizedExpression(e)) e = e.expression;
              }
              const el = ts.isJsxElement(e) ? e.openingElement
                : ts.isJsxSelfClosingElement(e) ? e : undefined;
              return !!el && el.tagName.getText() === 'button'
                && /btn--primary/.test(el.attributes.getText());
            });
            const demoted = dialog || footer || /menu__item|confirm-accept/.test(attrs);
            if (VERB.test(text.replace(/^[^A-Za-z]+/, '')) && !demoted) {
              const { line } = sf.getLineAndCharacterOfPosition(node.getStart());
              violations.push(`${f}:${line + 1} ${text.split('\n')[0].trim()}`);
            }
          }
          if (ts.isJsxElement(node)) nextRow = node;
        }
        node.forEachChild((c) => visit(c, dialog, nextRow));
      };
      visit(sf, false, undefined);
    }
    expect(
      violations,
      `destructive actions sitting at rest — move them into a RowMenu item:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  // One popup surface for the Action flyout, a row's overflow, the transcript
  // context menu and the picker list: the chrome is .menu, the placement is
  // the caller's own class.
  it('every menu draws from the one .menu surface', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const src = read(f);
      for (const m of src.matchAll(/role="(menu|menuitem)"/g)) {
        // The class list of the element carrying the role: scan back to its
        // opening angle bracket, forward to the closing one.
        const open = src.lastIndexOf('<', m.index);
        const close = src.indexOf('>', m.index);
        const tag = src.slice(open, close);
        // Class tokens from className alone — role="menu" must not supply
        // the token the rule is looking for — and split, since "ctx-menu"
        // is not "menu".
        const cls = /className=(?:"([^"]*)"|\{([^}]*)\})/.exec(tag);
        const classes = new Set(
          [...(cls?.[1] ?? cls?.[2] ?? '').matchAll(/[\w-]+/g)].map((t) => t[0]),
        );
        const want = m[1] === 'menu' ? 'menu' : 'menu__item';
        if (!classes.has(want)) {
          violations.push(`${f}:${lineOf(src, m.index)} role="${m[1]}" without the .${want} class`);
        }
      }
    }
    expect(
      violations,
      `menus drawing their own chrome — use .menu / .menu__item:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  // A route inside the rail wears the one PageHeader; a route rendering its own <h1> is flagged.
  it('a route in the rail wears the one PageHeader', () => {
    const app = readFileSync('src/App.tsx', 'utf8');
    const body = app.slice(app.indexOf('data-testid="app-body"'), app.indexOf('</main>'));
    // Two full-screen takeovers live in the rail's body without being pages:
    // the recorder is a centred card (idle) or the notebook (recording), and
    // the wizard is a stepped form reached from first run and from Settings.
    const TAKEOVERS = new Set(['ActiveSession', 'Wizard']);
    const routes = [...new Set([...body.matchAll(/<([A-Z]\w+)[\s/>]/g)].map((m) => m[1]))]
      .filter((n) => !TAKEOVERS.has(n));
    expect(routes.length).toBeGreaterThan(4);

    const violations: string[] = [];
    for (const name of routes) {
      const f = `src/routes/${name}.tsx`;
      if (!existsSync(f)) { violations.push(`${name} is mounted in the rail but has no src/routes/${name}.tsx`); continue; }
      const src = read(f);
      if (!src.includes('<PageHeader')) violations.push(`${f} renders no <PageHeader>`);
      const own = src.match(/<h1[\s>]/);
      if (own) violations.push(`${f}:${lineOf(src, own.index!)} writes its own <h1> — the title belongs to PageHeader`);
    }
    expect(
      violations,
      `routes inventing a page header:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  it('a settings group is named one way — the Group title', () => {
    // Every file that draws part of a Settings pane, not the route alone: a
    // pane split into its own component writes the same headings.
    const panes = FILES.filter((f) => /<(?:Group|SettingRow)\b|className="set-group"/.test(read(f)));
    expect(panes.length, 'no Settings panes found to judge').toBeGreaterThan(2);
    const violations: string[] = [];
    for (const f of panes) {
      const src = read(f);
      // A section's own name is carried by the selected tab and the page header,
      // so a heading inside the pane names a group, and every group names itself
      // through <Group title>. A dialog opened from Settings keeps .modal__title.
      for (const m of src.matchAll(/<h([1-6])\b/g)) {
        const attrs = tagAttrs(src, (m.index ?? 0) + m[0].length);
        const cls = attrs.match(/className="([^"]*)"/)?.[1] ?? '';
        // The engine sub-heads inside the transcription tuning panel are a rank
        // below a group title and name themselves.
        if (['set-group__title', 'modal__title', 'tune-panel__engine'].includes(cls)) continue;
        violations.push(`${f}:${lineOf(src, m.index ?? 0)} <h${m[1]} className="${cls}">`);
      }
      // The group surface itself is built once, by Group.
      const written = [...src.matchAll(/className="set-group"/g)];
      if (written.length && f !== 'src/components/SettingsGroup.tsx') {
        violations.push(`${f} builds .set-group itself — Group builds it`);
      }
    }
    expect(
      violations,
      `settings headings outside the one group idiom:\n${violations.join('\n')}`,
    ).toEqual([]);
  });

  it('the readability sweep expects every window main.tsx renders', () => {
    const main = readFileSync('src/main.tsx', 'utf8');
    const labels = [...main.matchAll(/label === '([\w-]+)'/g)].map((m) => m[1]);
    expect(labels.length, 'no window labels parsed out of main.tsx').toBeGreaterThan(1);

    const expected = SURFACES.surfaces;
    const missing = labels.filter((l) => !expected.some((e) => e === `window:${l}` || e.startsWith(`window:${l}-`)));
    expect(missing, `windows the readability sweep does not reach:\n${missing.join('\n')}`).toEqual([]);
  });

  it('the readability sweep expects every nav-rail route', () => {
    const app = readFileSync('src/App.tsx', 'utf8');
    const defs = app.slice(app.indexOf('export const NAV_DEFS'), app.indexOf('const DEFAULT_NAV_ORDER'));
    const routed = [...defs.matchAll(/\{ key: '([\w-]+)'[^}]*route: \{/g)].map((m) => m[1]);
    expect(routed.length, 'no routed nav items parsed out of App.tsx').toBeGreaterThan(3);

    const expected = new Set(SURFACES.surfaces);
    const missing = routed.filter((k) => !expected.has(`route:${k}`));
    expect(missing, `nav routes the readability sweep does not expect to reach:\n${missing.join('\n')}`)
      .toEqual([]);
  });

  // The brand doc quotes the tokens. A table column headed Light, Dark or Value quotes the declaration in that block; a More contrast column quotes the prefers-contrast: more override, and a dash says the block does not redeclare it.
  const BRAND = '../../docs/BRAND.md';
  it.skipIf(!existsSync(BRAND))(
    'docs/BRAND.md names only declared tokens, and its token tables quote the declarations',
    () => {
      const doc = readFileSync(BRAND, 'utf8');
      const css = readCss('src/styles/workspace.css');
      const block = (selector: string, media?: string) => {
        const from = media ? css.indexOf(`${media} {`) : 0;
        const at = from < 0 ? -1 : css.indexOf(`${selector} {`, from);
        if (at < 0) throw new Error(`no ${media ?? ''} ${selector} block in workspace.css`);
        const out = new Map<string, string>();
        for (const m of css.slice(at, css.indexOf('}', at)).matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
          out.set(m[1], m[2].replace(/\s+/g, ' ').trim());
        }
        return out;
      };
      const light = block(':root');
      const dark = block(':root[data-theme="dark"]');
      const MORE = '@media (prefers-contrast: more)';
      const columns: Record<string, (t: string) => string | undefined> = {
        Light: (t) => light.get(t),
        Value: (t) => light.get(t),
        Dark: (t) => dark.get(t) ?? light.get(t),
        'More contrast, light': (t) => block(':root', MORE).get(t),
        'More contrast, dark': (t) => block(':root[data-theme="dark"]', MORE).get(t),
      };

      const declared = new Set<string>();
      for (const f of [...SHEETS, 'src/styles/reset.css']) {
        for (const m of readFileSync(f, 'utf8').matchAll(/(--[\w-]+)\s*:/g)) declared.add(m[1]);
      }
      for (const f of FILES) {
        for (const m of read(f).matchAll(/['"`](--[\w-]+)['"`]\s*:/g)) declared.add(m[1]);
      }

      const violations: string[] = [];
      for (const span of doc.matchAll(/`[^`\n]+`/g)) {
        for (const m of span[0].matchAll(/(?<![\w-])--[\w-]+/g)) {
          if (!declared.has(m[0])) violations.push(`BRAND.md:${lineOf(doc, span.index ?? 0)} ${m[0]} is not declared`);
        }
      }

      let quoted = 0;
      const lines = doc.split('\n');
      const cellsOf = (l: string) => l.split('|').slice(1, -1).map((c) => c.trim());
      for (let i = 0; i < lines.length; i++) {
        if (!lines[i].startsWith('|') || lines[i - 1]?.startsWith('|')) continue;
        const head = cellsOf(lines[i]);
        const tokenCol = head.indexOf('Token');
        if (tokenCol < 0) continue;
        for (let r = i + 2; lines[r]?.startsWith('|'); r++) {
          const row = cellsOf(lines[r]);
          const where = `BRAND.md:${r + 1}`;
          const token = /^`(--[\w-]+)`$/.exec(row[tokenCol])?.[1];
          if (!token) {
            violations.push(`${where} the Token cell names no one token: ${row[tokenCol]}`);
            continue;
          }
          head.forEach((h, c) => {
            const actual = columns[h]?.(token);
            if (!(h in columns)) return;
            quoted++;
            if (row[c] === '—') {
              if (actual !== undefined) violations.push(`${where} ${token} ${h}: a dash, but the block declares ${actual}`);
              return;
            }
            const cited = /^`([^`]+)`$/.exec(row[c])?.[1];
            if (cited === undefined) violations.push(`${where} ${token} ${h}: quote the value in backticks, got ${row[c]}`);
            else if (cited !== actual) violations.push(`${where} ${token} ${h}: doc ${cited}, workspace.css ${actual ?? 'undeclared'}`);
          });
        }
      }
      expect(quoted, 'no token-table values parsed out of BRAND.md').toBeGreaterThan(20);
      expect(violations, `docs/BRAND.md disagrees with workspace.css:\n${violations.join('\n')}`).toEqual([]);
    },
  );
});
