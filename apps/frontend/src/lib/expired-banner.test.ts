// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { expiredBannerState } from './expired-banner';

describe('expiredBannerState', () => {
  it('returns hidden when license is null', () => {
    expect(expiredBannerState(null)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when license.state is not "expired" (trial)', () => {
    expect(expiredBannerState({ state: 'trial' })).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when license.state is not "expired" (licensed)', () => {
    expect(expiredBannerState({ state: 'licensed' })).toEqual({ kind: 'hidden' });
  });

  it('returns show with the exact banner text when license.state is "expired"', () => {
    expect(expiredBannerState({ state: 'expired' })).toEqual({
      kind: 'show',
      label: 'Your license has ended. Activate a license to record or import meetings.',
    });
  });
});
