// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
export type LicenseLike = { state: string } | null;

export type ExpiredBannerState =
  | { kind: 'hidden' }
  | { kind: 'show'; label: string };

const LABEL = 'Your license has ended. Activate a license to record or import meetings.';

export function expiredBannerState(license: LicenseLike): ExpiredBannerState {
  if (!license || license.state !== 'expired') return { kind: 'hidden' };
  return { kind: 'show', label: LABEL };
}
