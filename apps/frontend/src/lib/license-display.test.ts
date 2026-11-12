// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import type { LicenseStatus } from '../tauri';
import { perpetualUpgradeOffered, planName, revokedNotice } from './license-display';

const licensed = (perpetual: boolean, origin: string | null): LicenseStatus => ({
  state: 'licensed', name: 'Alex Example', email: 'alex@example.com', key: 'K', expires: 1,
  subscription_type: 'single_user', entitlements: [], perpetual, origin,
});

describe('license display', () => {
  it('names the plan as the phones do', () => {
    expect(planName('single_user')).toBe('Single user');
    expect(planName('')).toBe('—');
  });

  it('offers Buy Perpetual only on a Stripe or Google Play subscription', () => {
    expect(perpetualUpgradeOffered(licensed(false, 'stripe'))).toBe(true);
    expect(perpetualUpgradeOffered(licensed(false, 'google_play'))).toBe(true);
    expect(perpetualUpgradeOffered(licensed(true, 'stripe'))).toBe(false);
    expect(perpetualUpgradeOffered(licensed(false, 'app_store'))).toBe(false);
    expect(perpetualUpgradeOffered(licensed(false, null))).toBe(false);
    expect(perpetualUpgradeOffered({ state: 'expired', revoked_reason: 'replaced_by_lifetime' })).toBe(false);
  });

  it('asks for the Perpetual key after Perpetual replaced the subscription, and only then', () => {
    expect(revokedNotice({ state: 'expired', revoked_reason: 'replaced_by_lifetime' }))
      .toBe('This key was replaced by a Perpetual license. Enter the new key from your email.');
    expect(revokedNotice({ state: 'trial', days_left: 3, revoked_reason: 'replaced_by_lifetime' })).not.toBeNull();
    expect(revokedNotice({ state: 'expired', revoked_reason: 'chargeback' })).toBeNull();
    expect(revokedNotice({ state: 'expired' })).toBeNull();
    expect(revokedNotice(licensed(false, 'stripe'))).toBeNull();
  });
});
