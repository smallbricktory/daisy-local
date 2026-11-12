// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { RecordingSnapshot } from '../tauri';

const recordingSnapshot = vi.fn<() => Promise<RecordingSnapshot | null>>();

vi.mock('../tauri', () => ({
  tauri: { recordingSnapshot: () => recordingSnapshot() },
}));

let emit: ((e: { payload: RecordingSnapshot | null }) => void) | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (_event: string, cb: (e: { payload: RecordingSnapshot | null }) => void) => {
    emit = cb;
    return Promise.resolve(() => { emit = null; });
  },
}));

const { useRecordingState, getRecordingSnapshot, refreshRecordingSnapshot, releaseRecording } =
  await import('./recordingState');

const snap = (session_id: string, state: RecordingSnapshot['state']): RecordingSnapshot => ({
  state, session_id, session_root: `/s/${session_id}`, started_at_unix_seconds: 1_700_000_000,
  recorded_ms: 0, clock_running_since_unix_ms: 1_700_000_000_000,
  live_mode_label: 'off',
  single_local_speaker: true,
});

describe('recordingState', () => {
  it('a released session reads as not recording until the backend reports the stop', async () => {
    recordingSnapshot.mockResolvedValue(snap('daisy-1', 'paused'));
    const { result } = renderHook(() => useRecordingState());
    await waitFor(() => expect(result.current?.session_id).toBe('daisy-1'));

    act(() => releaseRecording('daisy-1'));
    expect(result.current).toBeNull();
    expect(getRecordingSnapshot()).toBeNull();

    // A read that lands before stop_recording returns still says paused.
    await act(() => refreshRecordingSnapshot());
    expect(recordingSnapshot).toHaveBeenCalledTimes(2);
    expect(result.current).toBeNull();

    // After the stop, a recording that reuses the id shows.
    act(() => emit?.({ payload: null }));
    act(() => emit?.({ payload: snap('daisy-1', 'recording') }));
    expect(result.current?.state).toBe('recording');
  });

  it('releasing a session the backend already stopped masks nothing', () => {
    act(() => emit?.({ payload: null }));
    act(() => releaseRecording('daisy-9'));
    act(() => emit?.({ payload: snap('daisy-9', 'recording') }));
    expect(getRecordingSnapshot()?.session_id).toBe('daisy-9');
  });

  it('a live-mode label change on the same session notifies', () => {
    act(() => emit?.({ payload: null }));
    act(() => emit?.({ payload: { ...snap('daisy-7', 'recording'), live_mode_label: 'Nemotron' } }));
    expect(getRecordingSnapshot()?.live_mode_label).toBe('Nemotron');
    act(() => emit?.({ payload: { ...snap('daisy-7', 'recording'), live_mode_label: 'off' } }));
    expect(getRecordingSnapshot()?.live_mode_label).toBe('off');
  });
});
