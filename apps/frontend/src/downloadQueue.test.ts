// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { DownloadFinished, DownloadJob } from './tauri';

const downloadQueueStatus = vi.fn<() => Promise<DownloadJob[]>>();
const downloadQueueEnqueue = vi.fn<(kind: string, id: string) => Promise<void>>();
const downloadQueueCancel = vi.fn<(id: string) => Promise<void>>();
const downloadQueueRetry = vi.fn<(id: string) => Promise<void>>();

vi.mock('./tauri', () => ({
  tauri: {
    downloadQueueStatus: () => downloadQueueStatus(),
    downloadQueueEnqueue: (kind: string, id: string) => downloadQueueEnqueue(kind, id),
    downloadQueueCancel: (id: string) => downloadQueueCancel(id),
    downloadQueueRetry: (id: string) => downloadQueueRetry(id),
  },
}));

let emit: ((e: { payload: DownloadJob[] }) => void) | null = null;
let emitFinished: ((e: { payload: DownloadFinished }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, cb: (e: { payload: unknown }) => void) => {
    if (event === 'download:finished') emitFinished = cb as typeof emitFinished;
    else emit = cb as typeof emit;
    return Promise.resolve(() => {});
  },
}));

const {
  useDownloadQueue, useDownloadJob, useDownloadFinished, explainDownloadError,
  enqueueDownload, cancelDownload, retryDownload, __resetDownloadQueueForTests,
} = await import('./downloadQueue');

const LLM = 'gemma-4-e4b-it-q4';
const job = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'llm', id: LLM, indeterminate: false, phase,
});

describe('downloadQueue', () => {
  beforeEach(() => {
    __resetDownloadQueueForTests();
    emit = null;
    downloadQueueStatus.mockReset().mockResolvedValue([]);
    downloadQueueEnqueue.mockReset().mockResolvedValue();
    downloadQueueCancel.mockReset().mockResolvedValue();
    downloadQueueRetry.mockReset().mockResolvedValue();
  });

  it('seeds from download_queue_status on the first subscription', async () => {
    downloadQueueStatus.mockResolvedValue([job({ state: 'queued' })]);
    const { result } = renderHook(() => useDownloadQueue());
    await waitFor(() => expect(result.current).toHaveLength(1));
    expect(result.current[0].id).toBe(LLM);
    expect(downloadQueueStatus).toHaveBeenCalledTimes(1);
  });

  it('follows download-queue:changed', async () => {
    const { result } = renderHook(() => useDownloadJob(LLM));
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { emit!({ payload: [job({ state: 'downloading', pct: 0.42 })] }); });
    expect(result.current?.phase).toEqual({ state: 'downloading', pct: 0.42 });
    act(() => { emit!({ payload: [job({ state: 'retrying', attempt: 2, of: 5, error: 'timed out' })] }); });
    expect(result.current?.phase).toEqual({ state: 'retrying', attempt: 2, of: 5, error: 'timed out' });
  });

  it('hands each ending to the view with its outcome', async () => {
    const seen: DownloadFinished[] = [];
    renderHook(() => useDownloadFinished((f) => seen.push(f)));
    await waitFor(() => expect(emitFinished).not.toBeNull());
    act(() => { emitFinished!({ payload: { id: LLM, kind: 'llm', outcome: 'cancelled' } }); });
    act(() => { emitFinished!({ payload: { id: LLM, kind: 'llm', outcome: 'installed' } }); });
    expect(seen.map((f) => f.outcome)).toEqual(['cancelled', 'installed']);
  });

  it('subscribes once no matter how many views read it', async () => {
    renderHook(() => useDownloadQueue());
    renderHook(() => useDownloadQueue());
    await waitFor(() => expect(downloadQueueStatus).toHaveBeenCalledTimes(1));
  });

  it('passes the commands straight through', async () => {
    await enqueueDownload('sa-assets', 'speechanalyzer');
    expect(downloadQueueEnqueue).toHaveBeenCalledWith('sa-assets', 'speechanalyzer');
    await cancelDownload(LLM);
    expect(downloadQueueCancel).toHaveBeenCalledWith(LLM);
    await retryDownload(LLM);
    expect(downloadQueueRetry).toHaveBeenCalledWith(LLM);
  });

  it('the first event wins the snapshot', async () => {
    let resolveStatus: ((jobs: DownloadJob[]) => void) | null = null;
    const statusPromise = new Promise<DownloadJob[]>((resolve) => {
      resolveStatus = resolve;
    });
    downloadQueueStatus.mockReturnValue(statusPromise);

    const { result } = renderHook(() => useDownloadQueue());
    await waitFor(() => expect(emit).not.toBeNull());

    act(() => {
      emit!({ payload: [job({ state: 'downloading', pct: 0.5 })] });
    });
    expect(result.current[0]?.phase).toEqual({ state: 'downloading', pct: 0.5 });

    act(() => {
      resolveStatus?.([job({ state: 'downloading', pct: 0.1 })]);
    });

    expect(result.current[0]?.phase).toEqual({ state: 'downloading', pct: 0.5 });
    expect(result.current).toHaveLength(1);
  });
});

// The strings the backend produces: `download model: <context chain>`, with
// the download URL and the profile path embedded in the context.
describe('explainDownloadError', () => {
  const MODELS = '/Users/alex/Library/Application Support/ai.daisy.Daisy/models';
  const URL = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin';

  it('names a missing model engine as a broken install, not a failed download', () => {
    const e = explainDownloadError(
      'built-in model worker missing at /Applications/Daisy.app/Contents/MacOS/daisy-llm',
    );
    expect(e.cause).toBe("Daisy's model engine is missing from this install");
    expect(e.advice).toBe('Reinstall Daisy.');
  });

  it('a chunk index that reads like an http status is not a server failure', () => {
    expect(
      explainDownloadError(`download model: open ${MODELS}/chunks/404: No such file or directory`).cause,
    ).not.toBe('Daisy could not reach the download server');
  });

  it('reads a full disk off the io error', () => {
    expect(
      explainDownloadError('download model: write tmp file: No space left on device (os error 28)').cause,
    ).toBe('There is not enough free space for this model');
  });

  it('reads an unwritable models folder off the io error', () => {
    expect(
      explainDownloadError(
        `download model: create ${MODELS}/ggml-base.en.bin.part: Permission denied (os error 13)`,
      ).cause,
    ).toBe('Daisy could not write to its models folder');
  });

  it('reads a refused connection and an error status as the server', () => {
    expect(
      explainDownloadError(
        `download model: GET ${URL}: error sending request for url (${URL}): error trying to connect: `
        + 'tcp connect error: Connection refused (os error 61)',
      ).cause,
    ).toBe('Daisy could not reach the download server');
    expect(
      explainDownloadError(
        `download model: GET ${URL} returned an error status: `
        + `HTTP status server error (503 Service Unavailable) for url (${URL})`,
      ).cause,
    ).toBe('Daisy could not reach the download server');
  });

  it('a longer number that spells a status code is not a status code', () => {
    expect(
      explainDownloadError('download model: write tmp file: wrote 1503 of 4096 bytes').cause,
    ).toBe('The download did not finish');
  });

  it('names a checksum mismatch as such and sends the user somewhere else', () => {
    const e = explainDownloadError(
      'download model: sha256 mismatch for base.en: got 921e4cf8, want a03779c8',
    );
    expect(e.cause).toBe('The downloaded file did not match the checksum Daisy has for it');
    expect(e.advice).toMatch(/different network/i);
    expect(e.advice).not.toMatch(/try the download again/i);
  });

  it('names a file longer than its pin and sends the user somewhere else', () => {
    const e = explainDownloadError(
      'download model: size mismatch: https://example.invalid/tokens.txt sent more than its pinned 8952 bytes',
    );
    expect(e.cause).toBe('The download server sent a larger file than Daisy expects');
    expect(e.advice).toMatch(/different network/i);
  });

  it('a local failure under a path that spells resolve is not a server failure', () => {
    expect(
      explainDownloadError(
        `download model: create ${MODELS}/resolve/ggml-base.en.bin.part: Is a directory (os error 21)`,
      ).cause,
    ).toBe('The download did not finish');
  });
});
