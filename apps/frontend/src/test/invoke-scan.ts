// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { readFileSync } from 'node:fs';
import { walk } from './walk';

/** Command names in plain and generic invoke calls with a quoted literal
 *  first argument, with the match index. The generic argument may nest. */
export function scanInvokes(s: string): { name: string; index: number }[] {
  const out: { name: string; index: number }[] = [];
  for (const m of s.matchAll(/\binvoke\b/g)) {
    let i = m.index + m[0].length;
    if (s[i] === '<') {
      let depth = 0;
      do {
        if (s[i] === '<') depth++;
        else if (s[i] === '>') depth--;
        i++;
      } while (depth > 0 && i < s.length);
    }
    const call = /^\(\s*'([a-z0-9_]+)'/.exec(s.slice(i));
    if (call) out.push({ name: call[1], index: m.index });
  }
  return out;
}

/** Every invoked command name under `dir` (`.ts`/`.tsx`, tests excluded),
 *  first occurrence wins: name → `file:line`. Node-only — the source-scan
 *  gates use it. */
export function invokedNames(dir: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const f of walk(dir, (n) => /\.tsx?$/.test(n) && !n.includes('.test.'))) {
    const s = readFileSync(f, 'utf8');
    for (const { name, index } of scanInvokes(s)) {
      if (!out.has(name)) out.set(name, `${f}:${s.slice(0, index).split('\n').length}`);
    }
  }
  return out;
}
