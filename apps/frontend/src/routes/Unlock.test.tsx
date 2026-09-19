// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
/**
 * Unlock screen: a profile whose vault opens only on another computer says
 * which computer, how to open it here, and offers another profile.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import type { VaultTrust } from '../tauri';

const openDialog = vi.fn();
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: (...a: unknown[]) => openDialog(...a) }));

const trust = vi.fn<() => Promise<VaultTrust>>();
const switchProfile = vi.fn<(p: string) => Promise<void>>();
vi.mock('../tauri', () => ({
  tauri: {
    buildInfo: vi.fn(() => Promise.resolve({ sha: 'abc1234' })),
    vaultTrust: () => trust(),
    unlockVault: vi.fn(() => Promise.reject(new Error('wrong'))),
    switchProfile: (p: string) => switchProfile(p),
    resetVault: vi.fn(() => Promise.resolve()),
  },
  errStr: (e: unknown) => String((e as { message?: unknown })?.message ?? e),
}));

import { Unlock, trustedComputer } from './Unlock';

beforeEach(() => {
  trust.mockReset();
  switchProfile.mockReset();
  openDialog.mockReset();
});

describe('Unlock', () => {
  it('asks for the passphrase when the vault is passphrase-protected', async () => {
    trust.mockResolvedValue({ kind: 'passphrase', trusted_on: null, this_machine: 'studio-mac' });
    render(<Unlock onUnlocked={() => {}} />);
    expect(await screen.findByText('Enter your vault passphrase.')).toBeInTheDocument();
    expect(screen.queryByTestId('unlock-elsewhere')).not.toBeInTheDocument();
    expect(screen.getByTestId('unlock-forgot')).toHaveTextContent('Forgot passphrase?');
  });

  it('names the computer a machine-mode vault belongs to and how to open it here', async () => {
    trust.mockResolvedValue({ kind: 'machine', trusted_on: 'fictional-laptop', this_machine: 'studio-mac' });
    render(<Unlock onUnlocked={() => {}} />);
    const box = await screen.findByTestId('unlock-elsewhere');
    expect(box).toHaveTextContent('only on fictional-laptop');
    expect(box).toHaveTextContent('Settings → Profile → Vault');
    expect(box).toHaveTextContent('Add a passphrase');
    expect(screen.getByText('Passphrase added on fictional-laptop')).toBeInTheDocument();
    expect(screen.getByTestId('unlock-forgot')).toHaveTextContent('No access to fictional-laptop?');
  });

  it('says this computer\'s key no longer opens a vault it owns, with no trip elsewhere', async () => {
    trust.mockResolvedValue({ kind: 'machine', trusted_on: 'studio-mac', this_machine: 'studio-mac' });
    render(<Unlock onUnlocked={() => {}} />);
    const box = await screen.findByTestId('unlock-key-lost');
    expect(box).toHaveTextContent("This computer's key no longer opens this profile.");
    expect(box).toHaveTextContent('If you added a passphrase');
    expect(screen.queryByTestId('unlock-elsewhere')).not.toBeInTheDocument();
    expect(screen.getByText('Passphrase')).toBeInTheDocument();
    expect(screen.getByTestId('unlock-other-profile')).toBeInTheDocument();
    fireEvent.click(screen.getByTestId('unlock-forgot'));
    expect(screen.getByTestId('unlock-reset-modal')).toBeInTheDocument();
    expect(screen.queryByTestId('unlock-reset-shared')).not.toBeInTheDocument();
  });

  it('names no other computer when the vault records no owner', async () => {
    trust.mockResolvedValue({ kind: 'machine', trusted_on: null, this_machine: 'studio-mac' });
    render(<Unlock onUnlocked={() => {}} />);
    const box = await screen.findByTestId('unlock-key-lost');
    expect(box).toHaveTextContent("This computer's key does not open this profile.");
    expect(screen.queryByTestId('unlock-elsewhere')).not.toBeInTheDocument();
  });

  it('warns that a reset reaches the other computer', async () => {
    trust.mockResolvedValue({ kind: 'machine', trusted_on: 'fictional-laptop', this_machine: 'studio-mac' });
    render(<Unlock onUnlocked={() => {}} />);
    fireEvent.click(await screen.findByText('No access to fictional-laptop?'));
    expect(screen.getByTestId('unlock-reset-shared')).toHaveTextContent('fictional-laptop loses the same keys');
  });

  it('opens a different profile folder from the lock screen', async () => {
    trust.mockResolvedValue({ kind: 'machine', trusted_on: 'fictional-laptop', this_machine: 'studio-mac' });
    openDialog.mockResolvedValue('/Users/alex/Daisy-new');
    switchProfile.mockResolvedValue(undefined);
    render(<Unlock onUnlocked={() => {}} />);
    fireEvent.click(await screen.findByTestId('unlock-other-profile'));
    await waitFor(() => expect(switchProfile).toHaveBeenCalledWith('/Users/alex/Daisy-new'));
  });

  it('shows why a profile switch failed', async () => {
    trust.mockResolvedValue({ kind: 'passphrase', trusted_on: null, this_machine: 'studio-mac' });
    openDialog.mockResolvedValue('/Users/alex/Daisy');
    switchProfile.mockRejectedValue(new Error('that is already the current profile directory'));
    render(<Unlock onUnlocked={() => {}} />);
    fireEvent.click(await screen.findByTestId('unlock-other-profile'));
    expect(await screen.findByTestId('unlock-switch-error')).toHaveTextContent('already the current profile');
  });
});

describe('trustedComputer', () => {
  it('uses the recorded name', () => {
    expect(trustedComputer({ kind: 'machine', trusted_on: 'fictional-laptop', this_machine: 'studio-mac' }))
      .toBe('fictional-laptop');
  });
  it('is null when no name is recorded, the name is this computer, or the vault has a passphrase', () => {
    expect(trustedComputer({ kind: 'machine', trusted_on: null, this_machine: 'studio-mac' })).toBeNull();
    expect(trustedComputer({ kind: 'machine', trusted_on: 'studio-mac', this_machine: 'studio-mac' })).toBeNull();
    expect(trustedComputer({ kind: 'passphrase', trusted_on: null, this_machine: 'studio-mac' })).toBeNull();
  });
});
