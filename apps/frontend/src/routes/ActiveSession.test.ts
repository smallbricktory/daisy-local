// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { liveBoost } from './ActiveSession';

describe('liveBoost', () => {
  it('boosts to the split max while the chat panel is open', () => {
    expect(liveBoost(true, false)).toBe(0.85);
  });

  it('an open chat panel wins over notes focus', () => {
    expect(liveBoost(true, true)).toBe(0.85);
  });

  it('boosts for notes focus when chat is closed', () => {
    expect(liveBoost(false, true)).toBe(0.66);
  });

  it('no boost when neither is active', () => {
    expect(liveBoost(false, false)).toBeNull();
  });
});
