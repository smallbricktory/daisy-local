export type ProviderStatus = {
  state: 'Configured' | 'Missing' | 'VaultLocked' | 'Unreachable' | 'None' | string;
  provider: string | null;
  hint: string | null;
};

export function summaryButtonsDisabled(s: ProviderStatus | null): boolean {
  if (!s) return false;
  return s.state === 'Missing' || s.state === 'Unreachable' || s.state === 'None';
}
