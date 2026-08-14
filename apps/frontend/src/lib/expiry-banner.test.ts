import { describe, it, expect } from 'vitest';
import { expiryBannerState } from './expiry-banner';

// Helpers — a fixed "now" to drive relative calculations.
const NOW_SEC = 1_700_000_000; // arbitrary fixed unix timestamp

describe('expiryBannerState', () => {
  it('returns hidden when license is null', () => {
    expect(expiryBannerState(null, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when license.state is not "licensed" (trial)', () => {
    expect(expiryBannerState({ state: 'trial' }, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when license.state is not "licensed" (expired)', () => {
    expect(expiryBannerState({ state: 'expired' }, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when expires is null (perpetual license)', () => {
    expect(expiryBannerState({ state: 'licensed', expires: null }, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when expires is undefined (perpetual license)', () => {
    expect(expiryBannerState({ state: 'licensed' }, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when expiryBannerDismissed is true', () => {
    const expires = NOW_SEC + 3 * 86_400; // 3 days from now
    expect(expiryBannerState({ state: 'licensed', expires }, NOW_SEC, true)).toEqual({ kind: 'hidden' });
  });

  it('returns hidden when daysLeft > 5', () => {
    const expires = NOW_SEC + 6 * 86_400; // 6 days from now
    expect(expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false)).toEqual({ kind: 'hidden' });
  });

  it('returns show with plural label when daysLeft === 5', () => {
    const expires = NOW_SEC + 5 * 86_400;
    const result = expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false);
    expect(result).toEqual({ kind: 'show', label: 'License expires in 5 days', daysLeft: 5 });
  });

  it('returns show with singular label when daysLeft === 1', () => {
    const expires = NOW_SEC + 1 * 86_400;
    const result = expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false);
    expect(result).toEqual({ kind: 'show', label: 'License expires in 1 day', daysLeft: 1 });
  });

  it('returns show with plural label when daysLeft === 2', () => {
    const expires = NOW_SEC + 2 * 86_400;
    const result = expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false);
    expect(result).toEqual({ kind: 'show', label: 'License expires in 2 days', daysLeft: 2 });
  });

  it('returns show with "License expired" when daysLeft === 0', () => {
    // expires exactly now — ceil((0) / 86400) === 0
    const expires = NOW_SEC;
    const result = expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false);
    expect(result).toEqual({ kind: 'show', label: 'License expired', daysLeft: 0 });
  });

  it('returns show with "License expired" when daysLeft < 0 (already expired)', () => {
    const expires = NOW_SEC - 2 * 86_400; // 2 days in the past
    const result = expiryBannerState({ state: 'licensed', expires }, NOW_SEC, false);
    expect(result.kind).toBe('show');
    if (result.kind === 'show') {
      expect(result.label).toBe('License expired');
      expect(result.daysLeft).toBeLessThan(0);
    }
  });
});
