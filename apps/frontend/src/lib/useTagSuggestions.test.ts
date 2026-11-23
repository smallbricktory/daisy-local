// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import type { SessionSuggestions } from '../tauri';

// One shared fake library bus we can fire at will — same pattern as
// useSessionData.test.ts.
let busHandlers: ((ev: { kind: string; session_id: string }) => void)[] = [];
vi.mock('./libraryEvents', () => ({
  subscribeToLibrary: (h: (ev: { kind: string; session_id: string }) => void) => {
    busHandlers.push(h);
    return () => { busHandlers = busHandlers.filter((x) => x !== h); };
  },
}));

// Controllable catalog — reassign `tagsArr` to a new array reference to
// simulate a catalog edit (create/rename/delete) and force a rerender.
let tagsArr: unknown[] = [];
vi.mock('./useTags', () => ({
  useTags: () => tagsArr,
}));

let n = 0;
let rejectNext = false;
vi.mock('../tauri', () => ({
  tauri: {
    tagSuggestForSession: vi.fn((sessionId: string) => {
      if (rejectNext) { rejectNext = false; return Promise.reject(new Error('boom')); }
      n += 1;
      return Promise.resolve({
        last_time: null,
        vocab: [{ tag_id: `${sessionId}-${n}`, total_hits: n, evidence: [] }],
      } as SessionSuggestions);
    }),
  },
}));

import { useTagSuggestions } from './useTagSuggestions';
import { tauri } from '../tauri';

function fireLibrary(session_id: string) {
  act(() => { for (const h of busHandlers) h({ kind: 'updated', session_id }); });
}

function calls() {
  return (tauri.tagSuggestForSession as ReturnType<typeof vi.fn>).mock.calls.length;
}

// The hook keeps a module-level cache keyed by session id, which outlives
// any single `it` block. Every test below uses a session id nothing else in
// this file touches, so a prior test's cached entry can never leak in.
let seq = 0;
function freshId(): string { seq += 1; return `sess-${seq}`; }

describe('useTagSuggestions', () => {
  beforeEach(() => { busHandlers = []; tagsArr = []; n = 0; rejectNext = false; vi.clearAllMocks(); });

  it('is null while sessionId is null and never fetches', () => {
    const { result } = renderHook(() => useTagSuggestions(null));
    expect(result.current).toBeNull();
    expect(calls()).toBe(0);
  });

  it('is null while loading, then resolves to the fetched suggestions', async () => {
    const id = freshId();
    const { result } = renderHook(() => useTagSuggestions(id));
    expect(result.current).toBeNull();
    await waitFor(() => expect(result.current).not.toBeNull());
    expect(result.current?.vocab[0].tag_id).toBe(`${id}-1`);
    expect(calls()).toBe(1);
  });

  it('reuses the module cache on remount for the same session — no refetch', async () => {
    const id = freshId();
    const first = renderHook(() => useTagSuggestions(id));
    await waitFor(() => expect(first.result.current).not.toBeNull());
    expect(calls()).toBe(1);
    first.unmount();

    const second = renderHook(() => useTagSuggestions(id));
    // Cached value is available synchronously — no loading flicker.
    expect(second.result.current?.vocab[0].tag_id).toBe(`${id}-1`);
    expect(calls()).toBe(1);
  });

  it('clears the cache and refetches when library:changed fires for its session', async () => {
    const id = freshId();
    const { result } = renderHook(() => useTagSuggestions(id));
    await waitFor(() => expect(result.current).not.toBeNull());
    expect(calls()).toBe(1);

    fireLibrary(id);
    await waitFor(() => expect(calls()).toBe(2));
    await waitFor(() => expect(result.current?.vocab[0].tag_id).toBe(`${id}-2`));
  });

  it('ignores library:changed for a different session', async () => {
    const id = freshId();
    const { result } = renderHook(() => useTagSuggestions(id));
    await waitFor(() => expect(result.current).not.toBeNull());
    expect(calls()).toBe(1);

    fireLibrary(freshId());
    await new Promise((r) => setTimeout(r, 20));
    expect(calls()).toBe(1);
  });

  it('refetches when the tag catalog identity changes', async () => {
    const id = freshId();
    const { result, rerender } = renderHook(() => useTagSuggestions(id));
    await waitFor(() => expect(result.current).not.toBeNull());
    expect(calls()).toBe(1);

    tagsArr = [{ id: 'new-tag' }]; // fresh reference — simulates a catalog edit
    rerender();
    await waitFor(() => expect(calls()).toBe(2));
    await waitFor(() => expect(result.current?.vocab[0].tag_id).toBe(`${id}-2`));
  });

  it('swallows a fetch error into null', async () => {
    const id = freshId();
    rejectNext = true;
    const { result } = renderHook(() => useTagSuggestions(id));
    await waitFor(() => expect(calls()).toBe(1));
    // No successful resolution ever lands — stays null.
    await new Promise((r) => setTimeout(r, 20));
    expect(result.current).toBeNull();
  });

  it('refetches when the session id changes', async () => {
    const idA = freshId();
    const idB = freshId();
    const { result, rerender } = renderHook(({ id }: { id: string }) => useTagSuggestions(id), {
      initialProps: { id: idA },
    });
    await waitFor(() => expect(result.current?.vocab[0].tag_id).toBe(`${idA}-1`));
    rerender({ id: idB });
    await waitFor(() => expect(result.current?.vocab[0].tag_id).toBe(`${idB}-2`));
  });
});
