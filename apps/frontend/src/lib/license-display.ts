// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { LicenseStatus } from '../tauri';

/** A plan as the license screens name it: `single_user` reads "Single user". */
export function planName(subscriptionType: string): string {
  const words = subscriptionType.replace(/_/g, ' ').trim();
  return words ? words[0].toUpperCase() + words.slice(1) : '—';
}

/** Whether Buy Perpetual is offered: an active Stripe or Google Play
 *  subscription, which the license server cancels when Perpetual replaces it. */
export function perpetualUpgradeOffered(status: LicenseStatus): boolean {
  return status.state === 'licensed' && !status.perpetual
    && (status.origin === 'stripe' || status.origin === 'google_play');
}

/** What the license screen says about the last key's revocation, until a
 *  new key activates; null for any reason it has nothing to add. The wire
 *  reason stays `replaced_by_lifetime`. */
export function revokedNotice(status: LicenseStatus): string | null {
  if (status.state === 'licensed') return null;
  return status.revoked_reason === 'replaced_by_lifetime'
    ? 'This key was replaced by a Perpetual license. Enter the new key from your email.'
    : null;
}
