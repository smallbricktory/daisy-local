// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, act, waitFor } from '@testing-library/react';
import { DownloadToast } from './DownloadToast';
import type { BuiltinRow, BuiltinStatus, DownloadFinished, DownloadJob } from '../tauri';

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
let emitFinished: ((e: { payload: DownloadFinished }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (event: string, cb: (e: { payload: unknown }) => void) => {
    if (event === 'download:finished') emitFinished = cb as typeof emitFinished;
    else emit = cb as typeof emit;
    return Promise.resolve(() => {});
  },
}));

const ROW: BuiltinRow = {
  id: 'gemma-4-e4b-it-q4', display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000,
  license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms',
  capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0, warming: false, warm_failed: null,
};
const status = (over: Partial<BuiltinRow>): BuiltinStatus => ({
  model: { ...ROW, ...over }, provider: { state: 'None', provider: null, hint: null },
});
let setBuiltin: ((s: BuiltinStatus) => void) | null = null;
vi.mock('../builtinModel', async () => {
  const { useState } = await import('react');
  return {
    useBuiltinModel: () => {
      const [s, set] = useState<BuiltinStatus>(status({}));
      setBuiltin = set;
      return s;
    },
  };
});

const { __resetDownloadQueueForTests } = await import('../downloadQueue');

const LLM = 'gemma-4-e4b-it-q4';
const llm = (phase: DownloadJob['phase']): DownloadJob => ({
  kind: 'llm', id: LLM, indeterminate: false, phase,
});
const ended = (outcome: DownloadFinished['outcome']) => {
  act(() => { emitFinished!({ payload: { id: LLM, kind: 'llm', outcome } }); });
};

/** Renders the driver once both of its events are subscribed. */
async function mount(): Promise<void> {
  render(<DownloadToast />);
  await waitFor(() => expect(emit).not.toBeNull());
  await waitFor(() => expect(emitFinished).not.toBeNull());
}

describe('DownloadToast', () => {
  beforeEach(() => {
    __resetDownloadQueueForTests();
    emit = null;
    emitFinished = null;
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

  it('replaces it with a short ready toast when the model is installed', async () => {
    await mount();
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.9 })] }); });
    act(() => { emit!({ payload: [] }); });
    ended('installed');
    act(() => { setBuiltin!(status({ installed: true })); });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'done',
      title: "Daisy's AI model is ready",
      autoDismissMs: 6000,
    })));
  });

  it('a cancelled download dismisses quietly, no ready toast', async () => {
    await mount();
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.1 })] }); });
    act(() => { emit!({ payload: [] }); });
    ended('cancelled');
    await waitFor(() => expect(dismissToast).toHaveBeenCalledWith('download-llm'));
    expect(pushToast).not.toHaveBeenCalledWith(expect.objectContaining({ severity: 'done' }));
  });

  it('the warm-up after the install keeps the toast working, then announces ready', async () => {
    await mount();
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.9 })] }); });
    act(() => { emit!({ payload: [] }); });
    ended('installed');
    act(() => { setBuiltin!(status({ installed: true, warming: true })); });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'working',
      body: 'Getting it ready…',
    })));
    act(() => { setBuiltin!(status({ installed: true, warming: false })); });
    await waitFor(() => expect(pushToast).toHaveBeenLastCalledWith(expect.objectContaining({
      id: 'download-llm',
      severity: 'done',
      title: "Daisy's AI model is ready",
    })));
  });

  it('a delete during the warm-up dismisses without announcing ready', async () => {
    render(<DownloadToast />);
    await waitFor(() => expect(emit).not.toBeNull());
    act(() => { setBuiltin!(status({ installed: true, warming: true })); });
    await waitFor(() => expect(pushToast).toHaveBeenCalled());
    act(() => { setBuiltin!(status({ installed: false, warming: false })); });
    await waitFor(() => expect(dismissToast).toHaveBeenCalledWith('download-llm'));
    expect(pushToast).not.toHaveBeenCalledWith(expect.objectContaining({ severity: 'done' }));
  });

  it('a job leaving the queue announces nothing until the backend says how it ended', async () => {
    await mount();
    act(() => { emit!({ payload: [llm({ state: 'downloading', pct: 0.9 })] }); });
    pushToast.mockClear();
    act(() => { emit!({ payload: [] }); });
    await act(async () => {});
    expect(pushToast).not.toHaveBeenCalled();
    expect(dismissToast).not.toHaveBeenCalled();
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
    await waitFor(() => expect(emitFinished).not.toBeNull());
    const raw = 'download model: GET https://example.invalid/shard.gguf returned an error status: 503';
    act(() => { emit!({ payload: [llm({ state: 'failed', error: raw })] }); });
    ended('failed');
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
