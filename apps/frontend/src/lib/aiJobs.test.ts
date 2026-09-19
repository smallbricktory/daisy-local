// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { AiJob } from '../tauri';

const aiJobsList = vi.fn<() => Promise<AiJob[]>>();

vi.mock('../tauri', () => ({
  tauri: { aiJobsList: () => aiJobsList() },
}));

let emit: ((e: { payload: AiJob[] }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (_event: string, cb: (e: { payload: AiJob[] }) => void) => {
    emit = cb;
    return Promise.resolve(() => { emit = null; });
  },
}));

const { useAiJobs, initAiJobs, runningJob, __resetAiJobsForTests } = await import('./aiJobs');

const job = (sessionId: string, kind: AiJob['kind']): AiJob => ({
  session_id: sessionId, kind, started_at_unix_seconds: 1_700_000_000,
});

describe('aiJobs', () => {
  beforeEach(() => {
    __resetAiJobsForTests();
    emit = null;
    aiJobsList.mockReset().mockResolvedValue([]);
  });

  it('initAiJobs seeds from ai_jobs_list', async () => {
    aiJobsList.mockResolvedValue([job('s-1', 'chapters')]);
    const { result } = renderHook(() => useAiJobs());
    await act(async () => { await initAiJobs(); });
    expect(result.current).toEqual([job('s-1', 'chapters')]);
    expect(aiJobsList).toHaveBeenCalledTimes(1);
  });

  it('a list that cannot be read leaves the registry empty', async () => {
    aiJobsList.mockRejectedValue(new Error('locked'));
    const { result } = renderHook(() => useAiJobs());
    await act(async () => { await initAiJobs(); });
    expect(result.current).toEqual([]);
  });

  it('follows ai-jobs:changed', async () => {
    const { result } = renderHook(() => useAiJobs());
    await act(async () => { await initAiJobs(); });
    await waitFor(() => expect(emit).not.toBeNull());

    act(() => { emit!({ payload: [job('s-2', 'summary')] }); });
    expect(result.current).toEqual([job('s-2', 'summary')]);

    act(() => { emit!({ payload: [] }); });
    expect(result.current).toEqual([]);
  });

  it('runningJob matches on session and kind', () => {
    const jobs = [job('s-1', 'chapters')];
    expect(runningJob(jobs, 's-1', 'chapters')).toEqual(jobs[0]);
    expect(runningJob(jobs, 's-1', 'summary')).toBeNull();
    expect(runningJob(jobs, 's-2', 'chapters')).toBeNull();
    expect(runningJob([], 's-1', 'chapters')).toBeNull();
  });
});
