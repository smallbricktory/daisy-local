// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { readdirSync } from 'node:fs';

/** Every file under `dir`, at any depth, whose basename passes `keep`. Paths
 *  come back forward-slash on every OS, so they compare against literal keys.
 *  A missing directory answers empty. Node-only — the source-scan gates use
 *  it. */
export function walk(dir: string, keep: (name: string) => boolean): string[] {
  const out: string[] = [];
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    const p = `${dir}/${e.name}`;
    if (e.isDirectory()) out.push(...walk(p, keep));
    else if (keep(e.name)) out.push(p);
  }
  return out;
}
