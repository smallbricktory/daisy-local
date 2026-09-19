// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { snoozeHelp } from './meeting-prompt';

describe('snoozeHelp', () => {
  it('says nothing without a snooze', () => {
    expect(snoozeHelp(null, Date.UTC(2026, 8, 16, 12, 0, 0))).toBe(null);
  });

  it('says nothing once the snooze has passed', () => {
    const now = Date.UTC(2026, 8, 16, 12, 0, 0);
    expect(snoozeHelp(Math.floor(now / 1000), now)).toBe(null);
    expect(snoozeHelp(Math.floor(now / 1000) - 60, now)).toBe(null);
  });

  it('names the local time the pause ends', () => {
    const now = Date.UTC(2026, 8, 16, 12, 0, 0);
    const until = new Date(2026, 8, 16, 15, 40, 0);
    const help = snoozeHelp(Math.floor(until.getTime() / 1000), now);
    expect(help).toBe(`Paused until ${until.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })}`);
  });
});
