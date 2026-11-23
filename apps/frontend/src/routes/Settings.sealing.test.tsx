// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
/**
 * Settings › Recordings: the row that follows the background seal of
 * recordings older builds left in plaintext.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, render, screen } from '@testing-library/react';

type Progress = { done: number; total: number };
const heard: { handler: ((e: { payload: Progress }) => void) | null } = { handler: null };
const NAME = 'encryption:progress';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((name: string, handler: (e: { payload: Progress }) => void) => {
    if (name !== NAME) return Promise.resolve(() => {});
    heard.handler = handler;
    return Promise.resolve(() => { heard.handler = null; });
  }),
}));
vi.mock('../components/MicLevel', () => ({ MicLevel: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../tauri', () => ({
  tauri: { encryptionStatus: vi.fn() },
  errStr: (e: unknown) => String(e),
  formatBytes: () => '0 B',
  LANGUAGES: [],
}));

import { SealProgressRow } from './Settings';
import { tauri as mockTauri } from '../tauri';
import { listen } from '@tauri-apps/api/event';
import { SEALING_PROGRESS } from '../encryptionCopy';

const status = (migration: Progress) => ({ content_key: 'ok' as const, words_pending: false, migration });

describe('SealProgressRow', () => {
  beforeEach(() => { heard.handler = null; });

  it('shows how far the seal has come, follows encryption:progress, and goes when it is done', async () => {
    vi.mocked(mockTauri.encryptionStatus).mockResolvedValue(status({ done: 3, total: 12 }));
    render(<SealProgressRow />);
    const row = await screen.findByTestId('seal-progress');
    expect(row.textContent).toContain(SEALING_PROGRESS.replace('{done}', '3').replace('{total}', '12'));
    expect(screen.getByRole('progressbar').getAttribute('aria-valuenow')).toBe('25');
    expect(listen).toHaveBeenCalledWith('encryption:progress', expect.any(Function));

    act(() => heard.handler!({ payload: { done: 11, total: 12 } }));
    expect(screen.getByTestId('seal-progress').textContent).toContain(
      SEALING_PROGRESS.replace('{done}', '11').replace('{total}', '12'),
    );
    act(() => heard.handler!({ payload: { done: 12, total: 12 } }));
    expect(screen.queryByTestId('seal-progress')).toBeNull();
  });

  it('shows nothing when there is nothing to seal', async () => {
    vi.mocked(mockTauri.encryptionStatus).mockResolvedValue(status({ done: 0, total: 0 }));
    render(<SealProgressRow />);
    await act(async () => { await Promise.resolve(); });
    expect(screen.queryByTestId('seal-progress')).toBeNull();
  });
});
