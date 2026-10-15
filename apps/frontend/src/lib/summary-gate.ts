// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
export type ProviderStatus = {
  state: 'Configured' | 'Missing' | 'Installing' | 'Unsupported' | 'VaultLocked' | 'Unreachable' | 'None' | string;
  provider: string | null;
  hint: string | null;
};

export function summaryButtonsDisabled(s: ProviderStatus | null): boolean {
  if (!s) return false;
  return s.state === 'Missing' || s.state === 'Installing' || s.state === 'Unreachable' || s.state === 'None' || s.state === 'Unsupported';
}
