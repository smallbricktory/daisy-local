import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';

// One shared fake library bus we can fire at will.
let busHandlers: ((ev: { kind: string; session_id: string }) => void)[] = [];
vi.mock('./libraryEvents', () => ({
  subscribeToLibrary: (h: (ev: { kind: string; session_id: string }) => void) => {
    busHandlers.push(h);
    return () => { busHandlers = busHandlers.filter((x) => x !== h); };
  },
}));

// Mocks the tauri data calls. Each returns an incrementing marker that tells
// reloads apart. `readSessionGate` lets a test hold a specific id's
// readSession pending, exercising the stale-response guard.
let n = 0;
let readSessionGate: ((id: string) => Promise<unknown> | null) | null = null;
vi.mock('../tauri', () => ({
  tauri: {
    readSession: vi.fn((id: string) => {
      const gated = readSessionGate?.(id);
      if (gated) return gated;
      return Promise.resolve({ session_id: id, transcript_md: `md${++n}` });
    }),
    sessionMetaGet: vi.fn(async () => ({ title: `t${n}`, attendees: [], tag_ids: [] })),
    summaryLoad: vi.fn(async () => null),
    listTags: vi.fn(async () => []),
    loadSessionChapters: vi.fn(async () => null),
    listSessionSpeakers: vi.fn(async () => []),
  },
  errStr: (e: unknown) => String(e),
}));

import { useSessionData } from './useSessionData';
import { tauri } from '../tauri';

function fireLibrary(session_id: string) {
  act(() => { for (const h of busHandlers) h({ kind: 'updated', session_id }); });
}

describe('useSessionData', () => {
  beforeEach(() => { busHandlers = []; n = 0; readSessionGate = null; vi.clearAllMocks(); });

  it('loads on mount', async () => {
    const { result } = renderHook(() => useSessionData('s1'));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    expect(result.current.view?.session_id).toBe('s1');
  });

  it('refetches when library:changed fires for its id', async () => {
    const { result } = renderHook(() => useSessionData('s1'));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    const before = (tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length;
    fireLibrary('s1');
    await waitFor(() =>
      expect((tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThan(before),
    );
  });

  it('ignores library:changed for a different id', async () => {
    const { result } = renderHook(() => useSessionData('s1'));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    const before = (tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length;
    fireLibrary('other');
    await new Promise((r) => setTimeout(r, 20));
    expect((tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length).toBe(before);
  });

  it('refetches on the synthetic stale signal (empty id)', async () => {
    const { result } = renderHook(() => useSessionData('s1'));
    await waitFor(() => expect(result.current.view).not.toBeNull());
    const before = (tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length;
    fireLibrary('');
    await waitFor(() =>
      expect((tauri.readSession as ReturnType<typeof vi.fn>).mock.calls.length).toBeGreaterThan(before),
    );
  });

  it('refetches when sessionId changes', async () => {
    const { result, rerender } = renderHook(({ id }) => useSessionData(id), {
      initialProps: { id: 's1' },
    });
    await waitFor(() => expect(result.current.view?.session_id).toBe('s1'));
    rerender({ id: 's2' });
    await waitFor(() => expect(result.current.view?.session_id).toBe('s2'));
  });

  it('drops a stale s1 fetch that resolves after switching to s2', async () => {
    // Hold s1's readSession pending; s2 resolves immediately.
    let resolveS1: (v: unknown) => void = () => {};
    readSessionGate = (id) =>
      id === 's1' ? new Promise((res) => { resolveS1 = res; }) : null;

    const { result, rerender } = renderHook(({ id }) => useSessionData(id), {
      initialProps: { id: 's1' },
    });
    // s1 is pending → no view yet. Switch to s2 (resolves immediately).
    rerender({ id: 's2' });
    await waitFor(() => expect(result.current.view?.session_id).toBe('s2'));

    // Now let the stale s1 fetch resolve — it must NOT clobber s2.
    await act(async () => {
      resolveS1({ session_id: 's1', transcript_md: 'stale' });
      await Promise.resolve();
    });
    expect(result.current.view?.session_id).toBe('s2');
  });
});
