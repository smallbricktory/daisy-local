import { describe, it, expect } from 'vitest';
import { transcriptTailSince } from './transcriptTail';
import type { TurnEntry, LiveTurn } from '../liveTranscript';

const turn = (p: Partial<LiveTurn>): LiveTurn => ({
  track: 'mic', text: 'hi', start_ms: 0, end_ms: 1000, isInterim: false, ...p,
});

describe('transcriptTailSince', () => {
  it('returns only finals newer than the cursor, labelled by track', () => {
    const finals: TurnEntry[] = [
      turn({ track: 'mic', text: 'old', end_ms: 500 }),
      turn({ track: 'system', text: 'new them', end_ms: 1500 }),
      turn({ track: 'mic', text: 'new me', end_ms: 2000 }),
    ];
    const tail = transcriptTailSince(finals, 1000);
    expect(tail.text).toBe('Others: new them\nMe: new me');
    expect(tail.endMs).toBe(2000);
  });

  it('skips interim turns and pause markers', () => {
    const finals: TurnEntry[] = [
      { marker: 'pause', at: 'now' },
      turn({ text: 'interim', end_ms: 1500, isInterim: true }),
      turn({ text: 'final', end_ms: 1600 }),
    ];
    const tail = transcriptTailSince(finals, 1000);
    expect(tail.text).toBe('Me: final');
    expect(tail.endMs).toBe(1600);
  });

  it('returns empty text and the original cursor when nothing is new', () => {
    const tail = transcriptTailSince([turn({ end_ms: 800 })], 1000);
    expect(tail.text).toBe('');
    expect(tail.endMs).toBe(1000);
  });

  it('drops blank lines but still advances the cursor', () => {
    const tail = transcriptTailSince([turn({ text: '   ', end_ms: 1200 })], 1000);
    expect(tail.text).toBe('');
    expect(tail.endMs).toBe(1200);
  });
});
