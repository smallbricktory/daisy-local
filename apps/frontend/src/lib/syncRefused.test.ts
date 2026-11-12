// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import type { SyncRefused } from '../tauri';
import { fmtDateTimeYear } from './format';
import { refusalRemedy, syncRefusedToast } from './syncRefused';

const AT = 1_790_963_366;
const ev = (over: Partial<SyncRefused>): SyncRefused => ({
  device_id: 'phone', session: 'daisy-1700000000', reason: 'unsupported', title: null, recorded_at_unix_seconds: AT, ...over,
});

describe('syncRefusedToast', () => {
  it('names an update only for a recording made with a newer Daisy', () => {
    const t = syncRefusedToast(ev({}));
    expect(t.body).toContain('newer Daisy');
    expect(t.body).toContain('Update Daisy on this computer to sync it.');
  });

  it('sends a recording whose audio does not decode back to the phone', () => {
    const t = syncRefusedToast(ev({ reason: 'undecodable' }));
    expect(t.body).toContain('missing playable audio');
    expect(t.body).toContain('Delete it on the phone to clear it.');
    expect(t.body).not.toContain('Update Daisy');
  });

  it('names the recording by its title and start time', () => {
    expect(syncRefusedToast(ev({ title: 'Standup' })).body).toMatch(new RegExp(`^“Standup” from ${fmtDateTimeYear(AT)} is `));
  });

  it('names an untitled recording by its start time, and one with neither as it', () => {
    expect(syncRefusedToast(ev({})).body).toMatch(new RegExp(`^The recording from ${fmtDateTimeYear(AT)} is `));
    expect(syncRefusedToast(ev({ title: 'Standup', recorded_at_unix_seconds: null })).body).toMatch(/^“Standup” is /);
    expect(syncRefusedToast(ev({ recorded_at_unix_seconds: null })).body).toMatch(/^It is /);
  });
});

describe('refusalRemedy', () => {
  it('speaks of one recording as it and several as them', () => {
    expect(refusalRemedy('sealed', 1)).toBe('Unpair the device and pair it again to sync it.');
    expect(refusalRemedy('sealed', 3)).toBe('Unpair the device and pair it again to sync them.');
  });
});
