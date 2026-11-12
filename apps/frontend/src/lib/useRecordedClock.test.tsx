// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { renderHook, render, act } from '@testing-library/react';
import type { RecordingSnapshot } from '../tauri';

let current: RecordingSnapshot | null = null;
vi.mock('./recordingState', async (orig) => ({
  ...(await orig<typeof import('./recordingState')>()),
  useRecordingState: () => current,
}));
vi.mock('../tauri', () => ({
  tauri: {},
  formatDurationSeconds: (s: number) => `t${s}`,
}));
vi.mock('./settingsStore', () => ({ useMachineSettings: () => null }));
vi.mock('./confirm', () => ({ confirm: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: () => Promise.resolve(() => {}) }));

const { useRecordedClock } = await import('./useRecordedClock');
const { recordedSeconds } = await import('./recordingState');
const { MiniWindow } = await import('../routes/MiniWindow');

const T0 = 1_700_000_000;
const at = (offset: number) => (T0 + offset) * 1000;

/** `recorded` seconds of audio, read at wall offset `since` (null: paused). */
const snap = (
  state: RecordingSnapshot['state'], recorded: number, since: number | null,
): RecordingSnapshot => ({
  state, session_id: 's', session_root: '/s', started_at_unix_seconds: T0,
  recorded_ms: recorded * 1000,
  clock_running_since_unix_ms: since === null ? null : at(since),
  live_mode_label: 'off',
  single_local_speaker: true,
});

describe('recordedSeconds', () => {
  it('adds the running segment to the closed ones', () => {
    expect(recordedSeconds(snap('recording', 0, 0), at(90))).toBe(90);
    expect(recordedSeconds(snap('recording', 60, 500), at(530))).toBe(90);
  });
  it('a paused snapshot reads its closed total at any time', () => {
    expect(recordedSeconds(snap('paused', 2, null), at(3000))).toBe(2);
  });
  it('a wall clock behind the segment start reads zero running time', () => {
    expect(recordedSeconds(snap('recording', 5, 100), at(90))).toBe(5);
  });
});

describe('recording clock', () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  function clockAt(offset: number) { vi.setSystemTime(at(offset)); }
  // The wall clock reads `offset` when the 1 s interval fires (fake timers
  // advance the system time with the timers).
  function tickAt(offset: number) {
    vi.setSystemTime(at(offset - 1));
    act(() => { vi.advanceTimersByTime(1000); });
  }

  it('pause freezes, resume continues from the frozen value, across several pauses', () => {
    clockAt(0);
    current = snap('recording', 0, 0);
    const { result, rerender } = renderHook(() => useRecordedClock(current));
    expect(result.current).toBe(0);

    tickAt(2);
    expect(result.current).toBe(2);

    // Paused at +2 s; the wall clock runs 28 min on.
    current = snap('paused', 2, null);
    rerender();
    expect(result.current).toBe(2);
    tickAt(1700);
    expect(result.current).toBe(2);

    // Resumed at +1700 s.
    current = snap('recording', 2, 1700);
    rerender();
    expect(result.current).toBe(2);
    tickAt(1710);
    expect(result.current).toBe(12);

    // A second pause and resume.
    current = snap('paused', 12, null);
    rerender();
    tickAt(5000);
    expect(result.current).toBe(12);
    current = snap('recording', 12, 5000);
    rerender();
    tickAt(5030);
    expect(result.current).toBe(42);
  });

  it('a mount mid-recording renders the recorded time on its first frame', () => {
    // A remount or webview reload while paused at 2 s, 28 min later.
    clockAt(1700);
    current = snap('paused', 2, null);
    const seen: number[] = [];
    renderHook(() => { const v = useRecordedClock(current); seen.push(v); return v; });
    expect(seen[0]).toBe(2);
  });

  it('a re-sync after a sleep brings the clock back to the audio', () => {
    // 570 s recorded; the machine sleeps 213 s and the wall clock runs on.
    clockAt(1000);
    current = snap('recording', 570, 1000);
    const { result, rerender } = renderHook(() => useRecordedClock(current));
    tickAt(1214);
    expect(result.current).toBe(784);
    // The backend reads 571 s of audio on disk and sends it.
    current = snap('recording', 571, 1214);
    rerender();
    expect(result.current).toBe(571);
    tickAt(1224);
    expect(result.current).toBe(581);
  });

  it('a cleared snapshot leaves the last value showing', () => {
    clockAt(0);
    current = snap('recording', 7, 0);
    const { result, rerender } = renderHook(() => useRecordedClock(current));
    expect(result.current).toBe(7);
    current = null;
    rerender();
    tickAt(100);
    expect(result.current).toBe(7);
  });

  it('the mini window shows recorded time, frozen while paused', () => {
    clockAt(0);
    current = snap('recording', 0, 0);
    const { container, rerender } = render(<MiniWindow />);
    const time = () => container.querySelector('.mini__time')?.textContent;
    tickAt(30);
    expect(time()).toBe('t30');

    current = snap('paused', 30, null);
    rerender(<MiniWindow />);
    tickAt(3000);
    expect(time()).toBe('t30');

    current = snap('recording', 30, 3000);
    rerender(<MiniWindow />);
    tickAt(3010);
    expect(time()).toBe('t40');
  });
});
