// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { mergeFinalInto, type TurnEntry, type LiveTurn } from './liveTranscript';

function mic(text: string, s: number, e: number): LiveTurn {
  return { track: 'mic', text, start_ms: s, end_ms: e, isInterim: false };
}
function sys(text: string, s: number, e: number): LiveTurn {
  return { track: 'system', text, start_ms: s, end_ms: e, isInterim: false };
}
const pause = (at: string): TurnEntry => ({ marker: 'pause', at });

describe('mergeFinalInto (incremental, store-level)', () => {
  it('merges a same-track final into the previous line (one growing caption)', () => {
    const out = mergeFinalInto([mic('Hello there', 0, 1000)], mic('how are you', 1500, 3000));
    expect(out).toHaveLength(1);
    expect(out[0]).toMatchObject({
      track: 'mic',
      text: 'Hello there how are you',
      start_ms: 0, // first segment's start kept (stable React key)
      end_ms: 3000, // extended to latest
    });
  });

  it('keeps a different-track final on its own line', () => {
    const out = mergeFinalInto([mic('I said this', 0, 1000)], sys('they replied', 1000, 2000));
    expect(out).toHaveLength(2);
    expect((out[1] as LiveTurn).text).toBe('they replied');
  });

  it('a pause marker breaks the merge — next final starts a new line', () => {
    const out = mergeFinalInto([mic('before', 0, 1000), pause('2026-06-15T00:00:00Z')], mic('after', 5000, 6000));
    expect(out).toHaveLength(3);
    expect((out[2] as LiveTurn).text).toBe('after');
  });

  it('appends to an empty transcript', () => {
    const out = mergeFinalInto([], mic('first', 0, 500));
    expect(out).toEqual([mic('first', 0, 500)]);
  });

  it('preserves references of unchanged earlier entries (keeps React.memo skips)', () => {
    const a = mic('a', 0, 1);
    const b = sys('b', 1, 2);
    const out = mergeFinalInto([a, b], sys('c', 2, 3));
    expect(out[0]).toBe(a); // unchanged entry — same reference
    expect(out[1]).not.toBe(b); // merged — new object
    expect((out[1] as LiveTurn).text).toBe('b c');
  });

  it('does not mutate the input array or entries', () => {
    const input = [mic('a', 0, 1)];
    mergeFinalInto(input, mic('b', 1, 2));
    expect(input).toHaveLength(1);
    expect((input[0] as LiveTurn).text).toBe('a');
  });
});
