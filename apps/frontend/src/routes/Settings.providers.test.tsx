// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
/**
 * ProvidersSection: with Nerd Mode off, only the currently active provider's
 * row stays visible among the cloud/self-hosted rows — a provider picked
 * while Nerd Mode was on must not become unreachable when it's turned off.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { MachineSettings, ProviderListEntry } from '../tauri';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../tauri', () => ({
  tauri: {
    listAsrModels: vi.fn(() => Promise.resolve([])),
    builtinModelStatus: vi.fn(() => new Promise(() => {})),
    downloadQueueStatus: vi.fn(() => Promise.resolve([])),
    downloadQueueEnqueue: vi.fn(() => Promise.resolve()),
    downloadQueueCancel: vi.fn(() => Promise.resolve()),
    downloadQueueRetry: vi.fn(() => Promise.resolve()),
    openExternal: vi.fn(),
  },
  errStr: (e: unknown) => String(e),
  formatBytes: () => '0 B',
  providerLabel: (p: string) => (p === 'daisy_gateway' ? 'Daisy Cloud (Internal use only)' : p),
}));

import { ProvidersSection } from './Settings';

const machine = {} as MachineSettings;

const row = (name: ProviderListEntry['name']): ProviderListEntry => ({
  name, has_key: false, model: null, base_url: null, context_tokens: null,
});

const base = {
  enable: vi.fn(),
  onSaved: vi.fn(),
  machine,
};

describe('ProvidersSection Nerd Mode visibility', () => {
  it('hides a non-active cloud provider row with Nerd Mode off', async () => {
    render(
      <ProvidersSection
        {...base}
        providerRows={[row('daisy_gateway')]}
        current={null}
        nerd={false}
      />,
    );
    await screen.findByText(/built-in model/);
    expect(screen.queryByTestId('provider-row-daisy_gateway')).not.toBeInTheDocument();
  });

  it('keeps the active cloud provider row visible with Nerd Mode off', async () => {
    render(
      <ProvidersSection
        {...base}
        providerRows={[row('daisy_gateway')]}
        current="daisy_gateway"
        nerd={false}
      />,
    );
    expect(await screen.findByTestId('provider-row-daisy_gateway')).toBeInTheDocument();
    expect(screen.getByTestId('provider-enabled-daisy_gateway')).toBeInTheDocument();
  });
});
