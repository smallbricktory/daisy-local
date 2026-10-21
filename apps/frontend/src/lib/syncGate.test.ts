// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, expect, it } from 'vitest';
import { outOfDateText, syncGatedToast } from './syncGate';

describe('the version gate words', () => {
  it('names the side that is out of date', () => {
    expect(outOfDateText('desktop', 'ios')).toBe(
      'Your desktop is out of date. Please update it to complete the sync. Sync has NOT been done.',
    );
    expect(outOfDateText('phone', 'ios')).toBe(
      'Your iOS device is out of date. Please update it to complete the sync. Sync has NOT been done.',
    );
    expect(outOfDateText('phone', 'android')).toBe(
      'Your Android device is out of date. Please update it to complete the sync. Sync has NOT been done.',
    );
  });

  it('raises one toast per device, replaced when the side changes', () => {
    const a = syncGatedToast({ device_id: 'phone-a1', platform: 'ios', out_of_date: 'phone' });
    const b = syncGatedToast({ device_id: 'phone-a1', platform: 'ios', out_of_date: 'desktop' });
    expect(a.id).toBe(b.id);
    expect(a.severity).toBe('warning');
    expect(b.body).toContain('Your desktop');
  });
});
