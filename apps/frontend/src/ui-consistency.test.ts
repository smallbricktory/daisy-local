// Static UI-consistency lint over the component sources. Fails on elements
// that bypass the design-system idioms (workspace.css tokens/classes).
import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

function tsxFiles(dir: string): string[] {
  const out: string[] = [];
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) out.push(...tsxFiles(p));
    else if (e.name.endsWith('.tsx') && !e.name.includes('.test.')) out.push(p);
  }
  return out;
}

const FILES = [...tsxFiles('src/routes'), ...tsxFiles('src/components')];

function read(f: string): string {
  // Line comments stripped so prose mentioning `<button>` is not scanned.
  return readFileSync(f, 'utf8').replace(/^\s*\/\/.*$/gm, '');
}

/** Attributes of a JSX tag starting at `start` (index after the tag name),
 *  brace-aware so `=>` inside handlers doesn't end the tag. */
function tagAttrs(s: string, start: number): string {
  let depth = 0;
  for (let i = start; i < s.length; i++) {
    const c = s[i];
    if (c === '{') depth++;
    else if (c === '}') depth--;
    else if (c === '>' && depth === 0) return s.slice(start, i);
  }
  return s.slice(start);
}

function lineOf(s: string, idx: number): number {
  return s.slice(0, idx).split('\n').length;
}

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

  it('headings use the .h1/.h2/.h3 classes', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      if (f.endsWith('MarkdownView.tsx')) continue; // renders user markdown
      const s = read(f);
      for (const m of s.matchAll(/<h([123])\b/g)) {
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
    const INTENTS: Array<{ verb: RegExp; requires?: string[]; exact?: string; forbids?: string[] }> = [
      { verb: /^Save$/, requires: ['btn--primary'] },
      { verb: /^Save as…?$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      { verb: /^Cancel$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      { verb: /^Edit$/, requires: ['btn'], forbids: ['btn--primary', 'btn--danger'] },
      { verb: /^Delete( \w+)?…?$/, requires: ['btn--danger'] },
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
        const close = s.indexOf('</button>', end);
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
          // Custom semantic controls stay exempt UNLESS nothing btn-like is
          // present at all — an intent verb on a hand-rolled button is
          // exactly the drift this rule exists to stop.
          const btnLike = clsBranches.some((c) => c.split(' ').includes('btn'));
          const semantic = clsBranches.length > 0 && clsBranches.every((c) => /__|--[a-z]/.test(c) && !c.split(' ').includes('btn'));
          if (!btnLike && semantic) continue;
          const ok = clsBranches.some((cls) =>
            (rule.exact ? cls === rule.exact : true)
            && (rule.requires ?? []).every((r) => cls.includes(r))
            && !(rule.forbids ?? []).some((r) => cls.includes(r)));
          if (!ok) violations.push(`${f}:${lineOf(s, j)} "${label}" [${clsBranches.join(' | ') || '(none)'}] fails intent rule ${rule.verb}`);
        }
        idx = j + 7;
      }
    }
    expect(violations, `intent-inconsistent buttons:\n${violations.join('\n')}`).toEqual([]);
  });

  it('text colors use tokens, not hardcoded black/white', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/color:\s*'(#000|#fff|black|white)'/gi)) {
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} (${m[1]})`);
      }
    }
    expect(violations, `hardcoded text colors (use var(--ink)/var(--cream-pure)):\n${violations.join('\n')}`).toEqual([]);
  });

  it('form controls keep the shared height — no inline padding/height/fontSize', () => {
    const EXEMPT_TYPES = /type="(checkbox|radio|range|file|color|hidden)"/;
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
      for (const tag of ['<select', '<input']) {
        let idx = 0;
        for (;;) {
          const j = s.indexOf(tag, idx);
          if (j < 0) break;
          const attrs = tagAttrs(s, j + tag.length);
          idx = j + tag.length;
          if (tag === '<input' && EXEMPT_TYPES.test(attrs)) continue;
          const ident = attrs.match(/style=\{([A-Za-z_$][\w$]*)\}/);
          if (ident) {
            violations.push(`${f}:${lineOf(s, j)} opaque style={${ident[1]}} on ${tag.slice(1)} (inline the layout props; sizing via workspace.css)`);
            continue;
          }
          const style = attrs.match(/style=\{\{([^]*?)\}\}/);
          if (!style) continue;
          if (/\.\.\./.test(style[1])) {
            violations.push(`${f}:${lineOf(s, j)} spread style on ${tag.slice(1)} (inline the layout props; sizing via workspace.css)`);
            continue;
          }
          const bad = style[1].match(/\b(padding|paddingTop|paddingBottom|height|fontSize|font)\s*:/);
          if (bad && !/height:\s*'auto'/.test(style[1]))
            violations.push(`${f}:${lineOf(s, j)} inline ${bad[1]} on ${tag.slice(1)} (size via workspace.css --control-h)`);
        }
      }
    }
    expect(violations, `inline-sized form controls:\n${violations.join('\n')}`).toEqual([]);
  });

  it('one button size — btn--sm is retired', () => {
    const violations: string[] = [];
    for (const f of FILES) {
      const s = read(f);
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
        const close = s.indexOf('</button>', end);
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
    let brandLockups = 0;
    // TSX: inline fontFamily must be a token or inherit.
    for (const f of FILES) {
      const s = read(f);
      for (const m of s.matchAll(/fontFamily:\s*(['"`])((?:(?!\1).)+)\1/g)) {
        const v = m[2];
        if (v === 'inherit' || /^var\(--font-(mono|display)\)$/.test(v)) continue;
        if (/^'Fraunces'/.test(v)) { brandLockups++; continue; } // About-page company wordmark
        violations.push(`${f}:${lineOf(s, m.index ?? 0)} fontFamily "${v}" (use var(--font-mono)/var(--font-display)/inherit)`);
      }
    }
    // CSS: font-family declarations are tokens, Inter stacks, or the two
    // whitelisted lockups (wordmark serif; token definitions in :root).
    const css = readFileSync('src/styles/workspace.css', 'utf8');
    let wordmarkSerifs = 0;
    for (const m of css.matchAll(/font-family:\s*([^;]+);/g)) {
      const v = m[1].trim();
      if (v.startsWith('var(--font-')) continue;
      if (/^"Inter"/.test(v)) continue;
      if (/^"(JetBrains Mono|Bricolage Grotesque)"/.test(v) && lineOf(css, m.index ?? 0) < 30) continue; // :root token defs
      if (/^"Cormorant Garamond"/.test(v)) { wordmarkSerifs++; continue; }
      if (v === 'inherit') continue;
      violations.push(`src/styles/workspace.css:${lineOf(css, m.index ?? 0)} font-family ${v}`);
    }
    if (wordmarkSerifs > 1) violations.push(`workspace.css: serif appears ${wordmarkSerifs}× — the brand wordmark is the only allowed use`);
    if (brandLockups > 1) violations.push(`Fraunces lockup appears ${brandLockups}× — the About company wordmark is the only allowed use`);
    expect(violations, `off-token typography:\n${violations.join('\n')}`).toEqual([]);
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
});
