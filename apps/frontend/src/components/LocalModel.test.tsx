// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { LocalModel } from './LocalModel';
import type { LlmModelInfo, DownloadJob } from '../tauri';

const listLlmModels = vi.fn<() => Promise<LlmModelInfo[]>>();
const deleteLlmModel = vi.fn<(id: string) => Promise<void>>();
const downloadQueueStatus = vi.fn<() => Promise<DownloadJob[]>>();
const downloadQueueEnqueue = vi.fn<(kind: string, id: string) => Promise<void>>();
const downloadQueueCancel = vi.fn<(id: string) => Promise<void>>();
const downloadQueueRetry = vi.fn<(id: string) => Promise<void>>();

vi.mock('../tauri', () => ({
  tauri: {
    listLlmModels: () => listLlmModels(),
    deleteLlmModel: (id: string) => deleteLlmModel(id),
    downloadQueueStatus: () => downloadQueueStatus(),
    downloadQueueEnqueue: (kind: string, id: string) => downloadQueueEnqueue(kind, id),
    downloadQueueCancel: (id: string) => downloadQueueCancel(id),
    downloadQueueRetry: (id: string) => downloadQueueRetry(id),
  },
  errStr: (e: unknown) => String(e),
  formatBytes: (n: number) => `${(n / 1_000_000).toFixed(0)} MB`,
}));

let emit: ((e: { payload: DownloadJob[] }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (_event: string, cb: (e: { payload: DownloadJob[] }) => void) => {
    emit = cb;
    return Promise.resolve(() => { emit = null; });
  },
}));

const { __resetDownloadQueueForTests } = await import('../downloadQueue');

const ID = 'gemma-4-e4b-it-q4';
const MODEL: LlmModelInfo = {
  id: ID, display: 'Gemma 4 E4B', installed: false, bytes: 4_000_000_000,
  license: 'Apache-2.0', license_url: 'https://example.test/license',
  capability: 'fast', capability_line: 'Fast on this machine', partial_bytes: 0,
};
const job = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'llm', id: ID, indeterminate: false, phase,
});

describe('LocalModel', () => {
  beforeEach(() => {
    __resetDownloadQueueForTests();
    emit = null;
    listLlmModels.mockReset().mockResolvedValue([MODEL]);
    deleteLlmModel.mockReset();
    downloadQueueStatus.mockReset().mockResolvedValue([]);
    downloadQueueEnqueue.mockReset().mockResolvedValue();
    downloadQueueCancel.mockReset().mockResolvedValue();
    downloadQueueRetry.mockReset().mockResolvedValue();
  });

  const props = { onChanged: () => {}, enabled: false, onEnable: () => {} };

  it('renders Queued for a job waiting its turn', async () => {
    render(<LocalModel {...props} />);
    await screen.findByTestId(`llm-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'queued' })] }); });
    await screen.findByText('Queued');
  });

  it('a job waiting to retry names the attempt to come and the last failure', async () => {
    render(<LocalModel {...props} />);
    await screen.findByTestId(`llm-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => {
      emit!({ payload: [job({ state: 'retrying', attempt: 3, of: 5, error: 'timed out' })] });
    });
    await screen.findByText('Retrying 3 of 5');
    const waiting = await screen.findByText('Attempt 2 failed: Daisy could not reach the download server.');
    expect(waiting).not.toHaveClass('meta--danger');
  });

  it('steps through a distinct percent before landing on the held one', async () => {
    render(<LocalModel {...props} />);
    await screen.findByTestId(`llm-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.1 })] }); });
    await screen.findByText('10%');
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.42 })] }); });
    await screen.findByText('42%');
  });

  it('a failed job offers Retry and Cancel to dismiss it', async () => {
    render(<LocalModel {...props} />);
    await screen.findByTestId(`llm-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'failed', error: 'no route to host' })] }); });
    const failed = await screen.findByText('Daisy could not reach the download server. Check your internet connection, then try again.');
    expect(failed).toHaveClass('meta--danger');
    const retry = await screen.findByTestId(`llm-download-${ID}`);
    expect(retry).toHaveTextContent('Retry download');
    const cancel = await screen.findByTestId(`llm-cancel-${ID}`);
    fireEvent.click(cancel);
    await waitFor(() => expect(downloadQueueCancel).toHaveBeenCalledWith(ID));
  });
});
