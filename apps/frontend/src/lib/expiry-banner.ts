export type LicenseLike = {
  state: 'licensed' | 'trial' | 'expired' | string;
  expires?: number | null;
};

export type ExpiryBannerState =
  | { kind: 'hidden' }
  | { kind: 'show'; label: string; daysLeft: number };

export function expiryBannerState(
  license: LicenseLike | null,
  nowSec: number,
  dismissed: boolean,
): ExpiryBannerState {
  if (!license || license.state !== 'licensed') return { kind: 'hidden' };
  if (license.expires == null) return { kind: 'hidden' };
  if (dismissed) return { kind: 'hidden' };
  const daysLeft = Math.ceil((license.expires - nowSec) / 86_400);
  if (daysLeft > 5) return { kind: 'hidden' };
  const label = daysLeft <= 0 ? 'License expired' : `License expires in ${daysLeft} day${daysLeft === 1 ? '' : 's'}`;
  return { kind: 'show', label, daysLeft };
}
