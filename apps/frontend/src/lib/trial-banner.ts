// Trial→paid conversion banner state (in-app, on-device; no telemetry/network).
// Sibling of expiry-banner.ts: a pure, unit-tested function that maps the local
// trial license + library stats to a staged banner. Two stages while still
// trialing — `gentle` (info, days ~21–26) and `urgent` (warning, days ~27–29).
// The 'ended' case is not here: at expiry the backend flips trial→expired
// and the app hard-gates to LicenseGate; a day-30 banner never renders.

export type TrialStats = { meetingCount: number };
export type TrialStage = 'gentle' | 'urgent';

export type TrialBannerState =
  | { kind: 'hidden' }
  | {
      kind: 'show';
      stage: TrialStage;
      bannerKind: 'info' | 'warning'; // maps to TopBanner.kind
      label: string; // value / loss-aversion framed
      daysLeft: number;
    };

/** Pricing anchor opened by the "Keep Daisy" CTA (and LicenseGate). No params. */
export const PRICING_URL = 'https://www.daisylocal.app/#pricing';

function plural(n: number): string {
  return n === 1 ? '' : 's';
}

export function trialBannerState(
  license: { state: string; days_left: number } | null,
  stats: TrialStats,
  dismissedStage: TrialStage | null,
): TrialBannerState {
  if (!license || license.state !== 'trial') return { kind: 'hidden' };
  const d = license.days_left;
  if (d > 9 || d <= 0) return { kind: 'hidden' }; // quiet early / gated at expiry

  const stage: TrialStage = d <= 3 ? 'urgent' : 'gentle';
  if (dismissedStage === stage) return { kind: 'hidden' };

  if (stage === 'urgent') {
    return {
      kind: 'show',
      stage,
      bannerKind: 'warning',
      daysLeft: d,
      label: `${d} day${plural(d)} left in your trial. Don't lose your setup — your meetings, voiceprints, and notes stay yours.`,
    };
  }

  // gentle
  const n = stats.meetingCount;
  const label =
    n > 0
      ? `You've recorded ${n} meeting${plural(n)} on this machine, kept fully private. Your trial ends in ${d} day${plural(d)} — keep them and keep recording.`
      : `Your trial ends in ${d} day${plural(d)}. Keep recording — everything stays on your machine.`;
  return { kind: 'show', stage, bannerKind: 'info', daysLeft: d, label };
}
