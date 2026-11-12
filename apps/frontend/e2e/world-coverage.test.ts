// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Every command the frontend invokes by name in src/ has a browser-mode
// handler. `plugin:*` commands are outside this check: their names live in
// @tauri-apps packages, not in src/, and the service resolves an unmocked
// `plugin:*` to undefined instead of failing.
/** @vitest-environment node */
import { describe, it, expect } from 'vitest';
import { invokedNames } from '../src/test/invoke-scan.ts';
import { makeWorld } from './fixtures/world.ts';

describe('browser-mode world', () => {
  it('mocks every invoke() name used in src/', () => {
    const have = new Set(Object.keys(makeWorld().handlers));
    const missing = [...invokedNames('src').keys()].filter((n) => !have.has(n));
    expect(missing.sort()).toEqual([]);
  });
});
