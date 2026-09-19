// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The frontend/backend command surface is a string contract: `invoke('name')`
// is matched to a Rust handler at runtime, so a rename or a typo fails in the
// app rather than at build time, and a handler nobody calls is dead weight
// that still compiles.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { invokedNames } from './test/invoke-scan';

const MAIN_RS = '../../crates/tauri-app/src/main.rs';

/** Command names inside `tauri::generate_handler![…]`. */
function registered(): Set<string> {
  const s = readFileSync(MAIN_RS, 'utf8');
  const open = s.indexOf('invoke_handler(tauri::generate_handler![');
  const body = s.slice(open, s.indexOf('])', open));
  return new Set(
    body
      .split('\n')
      .map((l) => l.trim().replace(/,$/, ''))
      .filter((l) => /^[a-z0-9_]+$/.test(l)),
  );
}

/** Names the frontend passes to invoke(). */
const invoked = () => invokedNames('src');

/** Handlers with no frontend caller, and why they are kept. */
const UNCALLED_OK = new Set<string>([]);

describe('tauri command contract', () => {
  it('every invoke() names a registered command', () => {
    const known = registered();
    const bad = [...invoked().entries()]
      .filter(([name]) => !known.has(name))
      .map(([name, at]) => `  ${name}  (called ${at})`);
    expect(
      bad.sort(),
      `invoke() names with no Rust handler — these fail at runtime:\n${bad.join('\n')}`,
    ).toEqual([]);
  });

  it('every registered command has a frontend caller', () => {
    const called = new Set(invoked().keys());
    const dead = [...registered()]
      .filter((c) => !called.has(c) && !UNCALLED_OK.has(c))
      .map((c) => `  ${c}`);
    expect(
      dead.sort(),
      `commands nothing invokes — delete, or record why in UNCALLED_OK:\n${dead.join('\n')}`,
    ).toEqual([]);
  });
});
