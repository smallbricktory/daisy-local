import { describe, it, expect } from 'vitest';
import { summaryButtonsDisabled, type ProviderStatus } from './summary-gate';

function makeStatus(state: ProviderStatus['state'], hint: string | null = null): ProviderStatus {
  return { state, provider: 'test-provider', hint };
}

describe('summaryButtonsDisabled', () => {
  it('is NOT disabled when Configured', () => {
    expect(summaryButtonsDisabled(makeStatus('Configured'))).toBe(false);
  });

  it('IS disabled when Missing', () => {
    expect(summaryButtonsDisabled(makeStatus('Missing'))).toBe(true);
  });

  it('is NOT disabled when VaultLocked (vault-locked = enabled, gentle hint)', () => {
    expect(summaryButtonsDisabled(makeStatus('VaultLocked'))).toBe(false);
  });

  it('IS disabled when Unreachable', () => {
    expect(summaryButtonsDisabled(makeStatus('Unreachable'))).toBe(true);
  });

  it('is NOT disabled when null (fail-open)', () => {
    expect(summaryButtonsDisabled(null)).toBe(false);
  });

  it('is NOT disabled for unknown state (fail-open)', () => {
    expect(summaryButtonsDisabled(makeStatus('SomeOtherState'))).toBe(false);
  });
});
