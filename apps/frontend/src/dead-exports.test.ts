// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// An exported symbol nothing imports is dead weight the compiler cannot see:
// `tsc` only reports unused *locals*, so an unused export survives forever.
import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) out.push(...sourceFiles(p));
    else if (/\.tsx?$/.test(e.name) && !e.name.endsWith('.d.ts')) out.push(p);
  }
  return out;
}

const ALL = sourceFiles('src');

/** Entry points and ambient files: nothing in src imports them by design. */
const ENTRY = /^src\/(main|vite-env)/;

describe('dead exports', () => {
  it('every exported symbol is referenced somewhere in src', () => {
    const corpus = ALL.map((f) => readFileSync(f, 'utf8')).join('\n');
    const violations: string[] = [];
    const DECL = /^export\s+(?:async\s+)?(?:default\s+)?(function|const|let|class|interface|type|enum)\s+([A-Za-z_]\w*)/gm;

    for (const f of ALL) {
      if (ENTRY.test(f)) continue;
      const s = readFileSync(f, 'utf8');
      for (const m of s.matchAll(DECL)) {
        const name = m[2];
        // One occurrence is the declaration itself.
        const uses = corpus.match(new RegExp(`\\b${name}\\b`, 'g'))?.length ?? 0;
        if (uses <= 1) {
          violations.push(`  ${f}:${s.slice(0, m.index ?? 0).split('\n').length}  ${m[1]} ${name}`);
        }
      }
    }
    expect(
      violations.sort(),
      `exported but never imported — delete, or drop the export keyword:\n${violations.join('\n')}`,
    ).toEqual([]);
  });
});
