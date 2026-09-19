// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, act, waitFor } from '@testing-library/react';
import { DownloadToast } from './DownloadToast';
import type { DownloadJob } from '../tauri';

const pushToast = vi.fn();
const dismissToast = vi.fn();
vi.mock('../lib/toastStore', () => ({
  pushToast: (spec: unknown) => pushToast(spec),
  dismissToast: (id: string) => dismissToast(id),
}));

const downloadQueueStatus = vi.fn<() => Promise<DownloadJob[]>>();
const downloadQueueRetry = vi.fn<(id: string) => Promise<void>>();
vi.mock('../tauri', () => ({
  tauri: {
    downloadQueueStatus: () => downloadQueueStatus(),
    downloadQueueEnqueue: () => Promise.resolve(),
    downloadQueueCancel: () => Promise.resolve(),
    downloadQueueRetry: (id: string) => downloadQueueRetry(id),
  },
}));

let emit: ((e: { payload: DownloadJob[] }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (_event: string, cb: (e: { payload: DownloadJob[] }) => void) => {
    emit = cb;
    return Promise.resolve(() => { emit = null; });
  },
}));

const { __resetDownloadQueueForTests, cancelDownload } = await import('../downloadQueue');

const LLM = 'gemma-4-e4b-it-q4';
const llm = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'llm', id: LLM, indeterminate: false, phase,
});

describe('DownloadToast', () => {
  beforeEach(() => {
    __resetDownloadQueueForTests();
    emit = null;
    pushToast.mockReset();
    dismissToast.mockReset();
    downloadQueueStatus.mockReset().mockResolvedValue([]);
    downloadQueueRetry.mockReset().mockResolvedValue();
  });

  it('shows one working toast with the percent while the model downloads', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.42 })] }); });
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'working',
      title: "Downloading Daisy's AI model",
      body: '42%',
      progress: 0.42,
    }));
  });

  it('replaces it with a short ready toast when the job leaves', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'warming' })] }); });
    act(() => { emit!({ payload: [] }); });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'done',
      title: "Daisy's AI model is ready",
      autoDismissMs: 6000,
    })));
  });

  it('a cancelled download dismisses quietly, no ready toast', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.1 })] }); });
    await act(async () => { await cancelDownload(LLM); });
    act(() => { emit!({ payload: [] }); });
    await waitFor(() => expect(dismissToast).toHaveBeenCalledWith('download-llm'));
    expect(pushToast).not.toHaveBeenCalledWith(expect.objectContaining({ severity: 'done' }));
  });

  it('a cancel during warm-up still announces ready', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'warming' })] }); });
    // Cancelling here does not stop the warm-up — the shards land and the
    // provider default is set regardless.
    await act(async () => { await cancelDownload(LLM); });
    act(() => { emit!({ payload: [] }); });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'done',
      title: "Daisy's AI model is ready",
      autoDismissMs: 6000,
    })));
  });

  it('a cancel mark survives a progress emit landing before removal', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.1 })] }); });
    await act(async () => { await cancelDownload(LLM); });
    // The backend has not removed the job yet — a progress emit still
    // carries the same id before the cancel actually lands.
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.2 })] }); });
    act(() => { emit!({ payload: [] }); });
    await waitFor(() => expect(dismissToast).toHaveBeenCalledWith('download-llm'));
    expect(pushToast).not.toHaveBeenCalledWith(expect.objectContaining({ severity: 'done' }));
  });

  it('a failed job offers Retry', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [llm({ state: 'failed', error: 'no route to host' })] }); });
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    const spec = pushToast.mock.calls.at(-1)![0] as {
      severity: string; body: string; actions: Array<{ label: string; onClick: () => void }>;
    };
    expect(spec.severity).toBe('error');
    expect(spec.body).toBe(
      'Daisy could not reach the download server. Check your internet connection, then try again.',
    );
    expect(spec.actions[0].label).toBe('Retry');
    spec.actions[0].onClick();
    await waitFor(() => expect(downloadQueueRetry).toHaveBeenCalledWith(LLM));
  });

  it('keeps the raw error out of the toast and puts it in the log', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    const raw = 'download model: GET https://example.invalid/shard.gguf returned an error status: 503';
    act(() => { emit!({ payload: [llm({ state: 'failed', error: raw })] }); });
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    const spec = pushToast.mock.calls.at(-1)![0] as { body: string };
    expect(spec.body).not.toContain('503');
    expect(spec.body).not.toContain('https://');
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('download failed'), LLM, raw);
    warn.mockRestore();
  });

  it('a download waiting to retry says so and names the attempt to come', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => {
      emit!({ payload: [llm({ state: 'retrying', attempt: 2, of: 5, error: 'timed out' })] });
    });
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'working',
      body: 'Trying again — attempt 2 of 5. Daisy could not reach the download server.',
    }));
  });

  it('the speech model gets its own toast', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => {
      emit!({ payload: [{
        kind: 'sa-assets', id: 'speechanalyzer',
        indeterminate: true, phase: { state: 'downloading', pct: 0 },
      }] });
    });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-asr',
      title: 'Downloading speech model',
      progress: undefined,
    })));
  });
});
