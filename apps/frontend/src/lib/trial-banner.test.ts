import { describe, it, expect } from 'vitest';
import { trialBannerState } from './trial-banner';

const trial = (days_left: number) => ({ state: 'trial', days_left });
const stats = (meetingCount = 3) => ({ meetingCount });

describe('trialBannerState', () => {
  it('hidden for non-trial / null license', () => {
    expect(trialBannerState(null, stats(), null).kind).toBe('hidden');
    expect(trialBannerState({ state: 'licensed', days_left: 5 }, stats(), null).kind).toBe('hidden');
    expect(trialBannerState({ state: 'expired', days_left: 0 }, stats(), null).kind).toBe('hidden');
  });

  it('hidden when more than 9 days left (quiet early)', () => {
    expect(trialBannerState(trial(10), stats(), null).kind).toBe('hidden');
  });

  it('hidden at/after expiry (defensive — backend gates)', () => {
    expect(trialBannerState(trial(0), stats(), null).kind).toBe('hidden');
    expect(trialBannerState(trial(-1), stats(), null).kind).toBe('hidden');
  });

  it('gentle (info) at days 9..4', () => {
    for (const d of [9, 6, 4]) {
      const s = trialBannerState(trial(d), stats(), null);
      expect(s.kind).toBe('show');
      if (s.kind === 'show') { expect(s.stage).toBe('gentle'); expect(s.bannerKind).toBe('info'); }
    }
  });

  it('urgent (warning) at days 3..1', () => {
    for (const d of [3, 2, 1]) {
      const s = trialBannerState(trial(d), stats(), null);
      expect(s.kind).toBe('show');
      if (s.kind === 'show') { expect(s.stage).toBe('urgent'); expect(s.bannerKind).toBe('warning'); }
    }
  });

  it('gentle dismissal hides gentle but NOT later urgent', () => {
    expect(trialBannerState(trial(6), stats(), 'gentle').kind).toBe('hidden');
    const urgent = trialBannerState(trial(2), stats(), 'gentle');
    expect(urgent.kind).toBe('show');
    if (urgent.kind === 'show') expect(urgent.stage).toBe('urgent');
  });

  it('urgent dismissal hides urgent', () => {
    expect(trialBannerState(trial(2), stats(), 'urgent').kind).toBe('hidden');
  });

  it('meetingCount === 0 uses fallback copy (never "0 meeting")', () => {
    const s = trialBannerState(trial(6), stats(0), null);
    expect(s.kind).toBe('show');
    if (s.kind === 'show') {
      expect(s.label).not.toMatch(/0 meeting/);
      expect(s.label).toMatch(/everything stays on your machine/);
    }
  });

  it('pluralizes days and meetings correctly', () => {
    const oneDay = trialBannerState(trial(1), stats(5), null);
    if (oneDay.kind === 'show') expect(oneDay.label).toMatch(/^1 day left/);
    const oneMeeting = trialBannerState(trial(6), stats(1), null);
    if (oneMeeting.kind === 'show') expect(oneMeeting.label).toMatch(/1 meeting on this machine/);
  });
});
