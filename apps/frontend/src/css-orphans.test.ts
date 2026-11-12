// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Checks the stylesheets and the components against each other in both
// directions: a class applied with no rule, and a rule nothing applies.
// Covers every stylesheet, including the mini and reminder popups' own.
import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const CSS_FILES = [
  'src/styles/workspace.css',
  'src/styles/reset.css',
  'src/mini-window.css',
  'src/reminder-window.css',
];

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) out.push(...sourceFiles(p));
    else if (/\.tsx?$/.test(e.name) && !e.name.includes('.test.')) out.push(p);
  }
  return out;
}

const SRC = sourceFiles('src');

function stripComments(s: string): string {
  return s.replace(/\/\*[\s\S]*?\*\//g, '');
}

function lineOf(s: string, idx: number): number {
  return s.slice(0, idx).split('\n').length;
}

/** Class names in selector position, i.e. the text preceding each `{`. */
function declaredClasses(): Map<string, string> {
  const where = new Map<string, string>();
  for (const f of CSS_FILES) {
    const css = stripComments(readFileSync(f, 'utf8'));
    for (const rule of css.matchAll(/([^{}]+)\{/g)) {
      const selector = rule[1];
      if (selector.trimStart().startsWith('@')) continue; // @media/@supports prelude
      for (const c of selector.matchAll(/\.(-?[A-Za-z_][\w-]*)/g)) {
        if (!where.has(c[1])) {
          where.set(c[1], `${f}:${lineOf(css, rule.index ?? 0)}`);
        }
      }
    }
  }
  return where;
}

/** Class tokens inside a string literal. */
/** A trailing fragment left by `foo--${x}` — a prefix, never a class. */
function isStem(t: string): boolean {
  return t.endsWith('-') || t.endsWith('_');
}

function split(text: string): string[] {
  return text
    .split(/[\s'"`]+/)
    .map((t) => t.trim())
    // Class names here are lowercase, hyphenated or BEM; a camelCase word is
    // an identifier from an expression.
    .filter((t) => /^-?[a-z_][\w-]*$/.test(t) && t.length > 1 && !/[A-Z]/.test(t));
}

/** Applied position: template holes are dropped, so an identifier inside
 *  `${…}` never reads as a class. */
function tokens(literal: string): string[] {
  return split(literal.replace(/\$\{[^{}]*\}/g, ' ')).filter((t) => !isStem(t));
}

/** Clearing position: holes are opened so a class written inside a ternary
 *  still counts as used. Stems are kept and matched as prefixes. */
function tokensLoose(literal: string): string[] {
  return split(literal.replace(/[${}]/g, ' '));
}

/**
 * Classes the components actually apply — className/class attributes and
 * classList calls. Deliberately narrow: this drives the "used but never
 * declared" failure, so a stray string elsewhere must not create a phantom.
 * Template holes (`${…}`) are dropped; their static neighbours still count.
 */
function appliedClasses(): Map<string, string> {
  const where = new Map<string, string>();
  const record = (name: string, at: string) => {
    if (!where.has(name)) where.set(name, at);
  };
  for (const f of SRC) {
    const s = readFileSync(f, 'utf8').replace(/^\s*\/\/.*$/gm, '');
    // className="…" | className={…} | classList.add('…', '…')
    const spots = [
      ...s.matchAll(/class(?:Name)?\s*=\s*"([^"]*)"/g),
      ...s.matchAll(/class(?:Name)?\s*=\s*\{([\s\S]{0,400}?)\}\s*(?:\n|\/?>|[a-zA-Z-]+=)/g),
      ...s.matchAll(/classList\.(?:add|remove|toggle)\(([^)]*)\)/g),
    ];
    for (const m of spots) {
      const body = m[1];
      const at = `${f}:${lineOf(s, m.index ?? 0)}`;
      if (m[0].includes('="')) {
        for (const t of tokens(body)) record(t, at);
        continue;
      }
      // `foo--${x}` leaves the stem `foo--`; that is a prefix, not a class.
      // Comparison operands are conditions, not classes.
      const values = body.replace(/[!=]==?\s*(['"`])[^'"`]*\1/g, '');
      for (const lit of values.matchAll(/'([^']*)'|"([^"]*)"|`([^`]*)`/g)) {
        for (const t of tokens(lit[1] ?? lit[2] ?? lit[3] ?? '')) record(t, at);
      }
    }
  }
  return where;
}

/**
 * Every class token appearing in any string literal in the sources. Used only
 * to clear the "declared but unused" direction, where a false accusation is
 * worse than a miss — a class assembled in a helper still counts as used.
 */
/** Prose-only modules whose string literals are English text, not class
 *  names; scanning them lets any dictionary-word class pass as "used". */
const CONTENT_ONLY = ['legalContent.ts', 'licenses.generated.ts'];

function mentionedAnywhere(): Set<string> {
  const seen = new Set<string>();
  for (const f of SRC) {
    if (CONTENT_ONLY.some((n) => f.endsWith(n))) continue;
    const s = readFileSync(f, 'utf8');
    for (const lit of s.matchAll(/'([^'\n]*)'|"([^"\n]*)"|`([^`]*)`/g)) {
      for (const t of tokensLoose(lit[1] ?? lit[2] ?? lit[3] ?? '')) seen.add(t);
    }
  }
  for (const f of ['index.html']) {
    try {
      for (const c of readFileSync(f, 'utf8').matchAll(/class="([^"]*)"/g)) {
        for (const t of tokensLoose(c[1])) seen.add(t);
      }
    } catch { /* no such file */ }
  }
  return seen;
}

/** Declared rules with no reference in the sources. Only the four below are
 *  exempt; a new rule must be applied by a component. */
const UNUSED_OK = new Set<string>([
  // Applied by main.tsx to <html>, not in JSX.
  'is-mini', 'is-reminder',
  // Third-party class, and an HTML attribute the scanner reads as one.
  'ProseMirror', 'scoped',
]);

/** Applied classes with no rule. Empty: every class has one. */
const UNDECLARED_OK = new Set<string>([]);

describe('CSS orphans', () => {
  it('every class the components apply has a rule somewhere', () => {
    const declared = declaredClasses();
    const applied = appliedClasses();
    const missing = [...applied.entries()]
      .filter(([c]) => !declared.has(c) && !UNDECLARED_OK.has(c))
      .map(([c, at]) => `  .${c}  (first applied ${at})`)
      .sort();
    expect(
      missing,
      `classes applied in JSX with no CSS rule — these render as unstyled native\n` +
        `elements and ignore the theme:\n${missing.join('\n')}`,
    ).toEqual([]);
  });

  it('every CSS rule is referenced by the components', () => {
    const declared = declaredClasses();
    const mentioned = mentionedAnywhere();
    const stems = [...mentioned].filter(isStem);
    const dead = [...declared.entries()]
      .filter(([c]) => !mentioned.has(c) && !UNUSED_OK.has(c)
        && !stems.some((s) => c.startsWith(s)))
      .map(([c, at]) => `  .${c}  (declared ${at})`)
      .sort();
    expect(
      dead,
      `CSS rules nothing references — delete them or wire them up:\n${dead.join('\n')}`,
    ).toEqual([]);
  });
});
