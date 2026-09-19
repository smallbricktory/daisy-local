// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { AsrModels } from './AsrModels';
import type { AsrModelInfo, DownloadJob } from '../tauri';

const listAsrModels = vi.fn<() => Promise<AsrModelInfo[]>>();
const setActiveAsrModel = vi.fn<(id: string) => Promise<void>>();
const deleteAsrModel = vi.fn<(id: string) => Promise<void>>();
const downloadQueueStatus = vi.fn<() => Promise<DownloadJob[]>>();
const downloadQueueEnqueue = vi.fn<(kind: string, id: string) => Promise<void>>();
const downloadQueueCancel = vi.fn<(id: string) => Promise<void>>();
const downloadQueueRetry = vi.fn<(id: string) => Promise<void>>();
const openExternal = vi.fn();

vi.mock('../tauri', () => ({
  tauri: {
    listAsrModels: () => listAsrModels(),
    setActiveAsrModel: (id: string) => setActiveAsrModel(id),
    deleteAsrModel: (id: string) => deleteAsrModel(id),
    downloadQueueStatus: () => downloadQueueStatus(),
    downloadQueueEnqueue: (kind: string, id: string) => downloadQueueEnqueue(kind, id),
    downloadQueueCancel: (id: string) => downloadQueueCancel(id),
    downloadQueueRetry: (id: string) => downloadQueueRetry(id),
    openExternal: (url: string) => openExternal(url),
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

const ID = 'parakeet-tdt-0.6b-v2-int8';
const PARAKEET: AsrModelInfo = {
  id: ID, engine: 'parakeet', display: 'Parakeet (English)',
  installed: false, active: false, bundled_floor: false, profile_copy: false,
  multilingual: false, bytes: 640_000_000, license: 'CC-BY-4.0',
  license_url: 'https://creativecommons.org/licenses/by/4.0/',
};
const job = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'asr', id: ID, indeterminate: false, phase,
});

const SA_ID = 'speechanalyzer';
const SPEECHANALYZER: AsrModelInfo = {
  id: SA_ID, engine: 'speechanalyzer', display: 'Apple Speech',
  installed: false, active: false, bundled_floor: false, profile_copy: false,
  multilingual: true, bytes: 0, license: '', license_url: '',
};
const saJob = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'sa-assets', id: SA_ID, indeterminate: true, phase,
});

const WHISPER_ID = 'whisper-base';
const WHISPER: AsrModelInfo = {
  id: WHISPER_ID, engine: 'whisper', display: 'Whisper (base)',
  installed: false, active: false, bundled_floor: false, profile_copy: false,
  multilingual: true, bytes: 140_000_000, license: 'MIT',
  license_url: 'https://opensource.org/licenses/MIT',
};

describe('AsrModels', () => {
  beforeEach(() => {
    __resetDownloadQueueForTests();
    emit = null;
    listAsrModels.mockReset().mockResolvedValue([PARAKEET]);
    setActiveAsrModel.mockReset();
    deleteAsrModel.mockReset();
    downloadQueueStatus.mockReset().mockResolvedValue([]);
    downloadQueueEnqueue.mockReset().mockResolvedValue();
    downloadQueueCancel.mockReset().mockResolvedValue();
    downloadQueueRetry.mockReset().mockResolvedValue();
    openExternal.mockReset();
  });

  it('Download enqueues the row and the queue drives the progress', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    fireEvent.click(screen.getByTestId(`asr-download-${ID}`));
    await waitFor(() => expect(downloadQueueEnqueue).toHaveBeenCalledWith('asr', ID));
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.42 })] }); });
    await screen.findByText('42%');
    expect(screen.getByTestId(`asr-cancel-${ID}`)).toBeInTheDocument();
  });

  it('survives unmount and remount mid-download', async () => {
    const first = render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    fireEvent.click(screen.getByTestId(`asr-download-${ID}`));
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.42 })] }); });
    await screen.findByText('42%');

    first.unmount();
    render(<AsrModels />);
    await screen.findByText('42%');
    expect(screen.queryByTestId(`asr-download-${ID}`)).not.toBeInTheDocument();
  });

  it('re-reads the rows when the job leaves the queue', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    fireEvent.click(screen.getByTestId(`asr-download-${ID}`));
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.9 })] }); });
    listAsrModels.mockResolvedValue([{ ...PARAKEET, installed: true, active: true, profile_copy: true }]);
    act(() => { emit!({ payload: [] }); });
    await screen.findByTestId(`asr-enabled-${ID}`);
  });

  it('a failed job shows its error and Retry', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'failed', error: 'no route to host' })] }); });
    const failed = await screen.findByText('Daisy could not reach the download server. Check your internet connection, then try again.');
    expect(failed).toHaveClass('meta--danger');
    const retry = await screen.findByTestId(`asr-download-${ID}`);
    expect(retry).toHaveTextContent('Retry download');
    fireEvent.click(retry);
    await waitFor(() => expect(downloadQueueRetry).toHaveBeenCalledWith(ID));
  });

  it('a failed job also offers Cancel to dismiss it', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'failed', error: 'no route to host' })] }); });
    const cancel = await screen.findByTestId(`asr-cancel-${ID}`);
    fireEvent.click(cancel);
    await waitFor(() => expect(downloadQueueCancel).toHaveBeenCalledWith(ID));
  });

  it('a failed speechanalyzer job also offers Cancel to dismiss it', async () => {
    listAsrModels.mockResolvedValue([SPEECHANALYZER]);
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${SA_ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [saJob({ state: 'failed', error: 'asset fetch failed' })] }); });
    const cancel = await screen.findByTestId(`asr-cancel-${SA_ID}`);
    fireEvent.click(cancel);
    await waitFor(() => expect(downloadQueueCancel).toHaveBeenCalledWith(SA_ID));
  });

  it('an active speechanalyzer job stays uncancellable', async () => {
    listAsrModels.mockResolvedValue([SPEECHANALYZER]);
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${SA_ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [saJob({ state: 'downloading', pct: 0 })] }); });
    await screen.findByText('downloading…');
    expect(screen.queryByTestId(`asr-cancel-${SA_ID}`)).not.toBeInTheDocument();
  });

  it('renders Queued for a job waiting its turn', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'queued' })] }); });
    await screen.findByText('Queued');
  });

  it('a job waiting to retry names the attempt to come and the last failure', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => {
      emit!({ payload: [job({ state: 'retrying', attempt: 3, of: 5, error: 'timed out' })] });
    });
    await screen.findByText('Retrying 3 of 5');
    const waiting = await screen.findByText('Attempt 2 failed: Daisy could not reach the download server.');
    expect(waiting).not.toHaveClass('meta--danger');
  });

  it('Cancel drops the job through the queue', async () => {
    render(<AsrModels />);
    await screen.findByTestId(`asr-download-${ID}`);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.1 })] }); });
    fireEvent.click(await screen.findByTestId(`asr-cancel-${ID}`));
    await waitFor(() => expect(downloadQueueCancel).toHaveBeenCalledWith(ID));
  });

  it('Whisper is hidden without Nerd Mode and shown with it', async () => {
    listAsrModels.mockResolvedValue([PARAKEET, WHISPER]);
    const { rerender } = render(<AsrModels nerd={false} />);
    await screen.findByTestId(`asr-download-${ID}`);
    expect(screen.queryByTestId(`asr-download-${WHISPER_ID}`)).not.toBeInTheDocument();

    rerender(<AsrModels nerd />);
    await screen.findByTestId(`asr-download-${WHISPER_ID}`);
  });

  it('an active Whisper model stays visible without Nerd Mode', async () => {
    listAsrModels.mockResolvedValue([
      PARAKEET,
      { ...WHISPER, installed: true, active: true, profile_copy: true },
    ]);
    render(<AsrModels nerd={false} />);
    await screen.findByTestId(`asr-enabled-${WHISPER_ID}`);
  });
});
