// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Every interactive element carries data-testid so the e2e page objects can
// address it. Static scan over the component sources.
/** @vitest-environment node */
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { walk } from './test/walk';
import { tagAttrs } from './test/jsx-scan';

/** Every component source under `src/`, at any depth: a new directory is
 *  covered the moment it is added. */
const FILES = walk('src', (n) => n.endsWith('.tsx') && !n.includes('.test.'));

/** `file id` pairs written twice in one file on mutually-exclusive branches, so
 *  only one is ever in the document. */
const EXCLUSIVE_IDS = new Set<string>([
  'src/routes/Wizard.tsx wizard-mic-continue',
  // Heading when idle, text input while renaming.
  'src/routes/SummaryPane.tsx session-title',
  // In place of the page while it cannot load, under it once it has.
  'src/routes/Settings.tsx settings-error',
  // One per tab/branch: the notes tab and SummaryTab's four early
  // returns (paste mode, no-summary, editing, viewing). `tab` is a
  // single value and each SummaryTab branch is its own `return`, so at
  // most one of the five ever renders.
  'src/routes/SummaryPane.tsx session-error',
]);

const READ_CACHE = new Map<string, string>();
function read(f: string): string {
  let s = READ_CACHE.get(f);
  if (s === undefined) {
    s = readFileSync(f, 'utf8').replace(/^\s*\/\/.*$/gm, '').replace(/\{\/\*[\s\S]*?\*\/\}/g, '');
    READ_CACHE.set(f, s);
  }
  return s;
}

const lineOf = (s: string, i: number) => s.slice(0, i).split('\n').length;

/** Interactive = form control tag, or any tag carrying onClick/onChange. */
function untagged(f: string): string[] {
  const s = read(f);
  const out: string[] = [];
  const re = /<([A-Za-z][\w.]*)\b/g;
  for (const m of s.matchAll(re)) {
    const tag = m[1];
    const attrs = tagAttrs(s, m.index! + m[0].length);
    const formControl = /^(button|input|select|textarea)$/.test(tag);
    const handler = /\bon(Click|Change|KeyDown|Submit)=/.test(attrs) && /^[a-z]/.test(tag);
    if (!formControl && !handler) continue;
    if (/\bdata-testid=/.test(attrs)) continue;
    // A listbox option is addressed by its value inside the picker that owns
    // it — `[data-testid="<picker>"] ~ [role="listbox"] [data-value="..."]`.
    if (/\brole="option"/.test(attrs) && /\bdata-value=/.test(attrs)) continue;
    out.push(`${f}:${lineOf(s, m.index!)} <${tag}>`);
  }
  return out;
}

describe('testid coverage', () => {
  it('every interactive element has data-testid', () => {
    const bad = FILES.flatMap(untagged);
    expect(bad).toEqual([]);
  });

  it('no duplicate static ids within one file', () => {
    const dups: string[] = [];
    for (const f of FILES) {
      const seen = new Map<string, number>();
      for (const m of read(f).matchAll(/data-testid="([^"]+)"/g)) seen.set(m[1], (seen.get(m[1]) ?? 0) + 1);
      for (const [id, n] of seen) if (n > 1 && !EXCLUSIVE_IDS.has(`${f} ${id}`)) dups.push(`${f} ${id} x${n}`);
    }
    expect(dups).toEqual([]);
  });
});

const tsFiles = (dir: string) => walk(dir, (n) => n.endsWith('.ts') && !n.includes('.test.'));

/** Ids the suite must reach: every static `data-testid="x"` and the literal
 *  head of every template `data-testid={`x-${…}`}` under `src/`. */
function srcIds(): { exact: Map<string, string>; prefix: Map<string, string> } {
  const exact = new Map<string, string>();
  const prefix = new Map<string, string>();
  for (const f of FILES) {
    const s = read(f);
    for (const m of s.matchAll(/data-testid="([^"]+)"/g)) exact.set(m[1], `${f}:${lineOf(s, m.index!)}`);
    for (const m of s.matchAll(/data-testid=\{`([^`$]+)\$\{/g)) prefix.set(m[1], `${f}:${lineOf(s, m.index!)}`);
  }
  return { exact, prefix };
}

/** A quoted id, or the literal head of a template one: `'x'` | `` `x-${ ``. */
const ID_ARG = String.raw`(?:'([^']+)'|\x60([^\x60$]+)\$\{)`;
/** `e2e/pages/_shared.ts` helpers and `Scope` methods that take one id. */
const ONE = new RegExp(
  String.raw`\b(?:tid|typeInto|selectOption|setNativeValue|optionValueByText|clickScrim|expectRowText|expectText|one)\(\s*${ID_ARG}`,
  'g',
);
const ONE_IN = new RegExp(String.raw`\bwithin\([^,]+,\s*${ID_ARG}`, 'g');
/** Those that take a prefix. */
const MANY = new RegExp(String.raw`\b(?:tids|idByText|idsOf|expectRowWithText|many)\(\s*${ID_ARG}`, 'g');
const MANY_IN = new RegExp(String.raw`\bwithinAll\([^,]+,\s*${ID_ARG}`, 'g');
const CSS_ONE = new RegExp(String.raw`\[data-testid="([^"$]+)(?:"\]|\$\{)`, 'g');
const CSS_MANY = new RegExp(String.raw`\[data-testid\^="([^"$]+)(?:"\]|\$\{)`, 'g');

/** Ids the suite addresses, through the helpers above or a raw CSS selector. */
function e2eRefs(): { exact: Set<string>; prefix: Set<string> } {
  const exact = new Set<string>();
  const prefix = new Set<string>();
  for (const f of [...tsFiles('e2e/pages'), ...tsFiles('e2e/specs')]) {
    const s = readFileSync(f, 'utf8');
    for (const re of [ONE, ONE_IN]) {
      for (const m of s.matchAll(re)) (m[1] ? exact : prefix).add(m[1] ?? m[2]);
    }
    for (const re of [MANY, MANY_IN]) {
      for (const m of s.matchAll(re)) prefix.add(m[1] ?? m[2]);
    }
    for (const m of s.matchAll(CSS_ONE)) (m[0].endsWith('"]') ? exact : prefix).add(m[1]);
    for (const m of s.matchAll(CSS_MANY)) prefix.add(m[1]);
  }
  return { exact, prefix };
}

describe('testid reach', () => {
  it('every id in src/ is addressed by a page object or spec', () => {
    const { exact, prefix } = srcIds();
    const refs = e2eRefs();
    const refExacts = [...refs.exact];
    const refPrefixes = [...refs.prefix];
    const reached = (id: string) => refs.exact.has(id) || refPrefixes.some((p) => id.startsWith(p));
    const reachedPrefix = (p: string) =>
      refExacts.some((id) => id.startsWith(p)) ||
      refPrefixes.some((q) => p.startsWith(q) || q.startsWith(p));
    const orphans: string[] = [];
    for (const [id, at] of exact) if (!reached(id)) orphans.push(`${at} ${id}`);
    for (const [p, at] of prefix) if (!reachedPrefix(p)) orphans.push(`${at} ${p}\${…}`);
    expect(orphans.sort()).toEqual([]);
  });
});
