// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { closed, endsSentence, groupTurns, join, readsOn, sentenceCount, type RuleLine } from './turnRule';

// Same cases as `transcript::reading` and the phone's TranscriptSplit tests.
describe('sentence rules', () => {
  it('counts sentences by the end rules', () => {
    expect(sentenceCount('')).toBe(0);
    expect(sentenceCount('and then we')).toBe(0);
    expect(sentenceCount('Hello there. How are you? Fine!')).toBe(3);
    expect(sentenceCount('Ask Dr. Baylor about version 3.5 with John F. Kennedy')).toBe(0);
    expect(sentenceCount('Ask Dr. Baylor. He knows.')).toBe(2);
    expect(sentenceCount('He said "go." Then left.')).toBe(2);
    expect(sentenceCount('你好。我是小明。谢谢！')).toBe(3);
    expect(sentenceCount('今日は晴れです。散歩に行きますか？ Fine.')).toBe(3);
  });

  it('ends a sentence at a stop that closes no title or initial', () => {
    expect(endsSentence('We shipped it.')).toBe(true);
    expect(endsSentence('So did I.')).toBe(true);
    expect(endsSentence('Really?" ')).toBe(true);
    expect(endsSentence('Talk to Dr.')).toBe(false);
    expect(endsSentence('John F.')).toBe(false);
    expect(endsSentence('and then we')).toBe(false);
  });

  it('joins an unfinished sentence across any pause unless another voice says it', () => {
    expect(readsOn(90_000, 1, 'and then we', 1_000, 1)).toBe(true);
    expect(readsOn(90_000, null, 'and then we', 1_000, 1)).toBe(true);
    expect(readsOn(1_100, 2, 'and then we', 1_000, 1)).toBe(false);
  });

  it('joins whole sentences of one voice within the gap', () => {
    expect(readsOn(3_000, 1, 'Done.', 1_000, 1)).toBe(true);
    expect(readsOn(3_001, 1, 'Done.', 1_000, 1)).toBe(false);
    expect(readsOn(1_100, 1, 'Done.', 1_000, null)).toBe(false);
  });

  it('closes a caption past its size at a sentence end, a saved turn only at three sentences', () => {
    const long = `${'word '.repeat(300)}end.`;
    expect(closed(long, true)).toBe(true);
    expect(closed(long, false)).toBe(false);
    expect(closed(`${'word '.repeat(300)}and`, true)).toBe(false);
    expect(closed('One. Two. Three.', false)).toBe(true);
  });

  it('spaces words except in scripts written without spaces', () => {
    expect(join('Hello', 'there')).toBe('Hello there');
    expect(join('你好', '谢谢')).toBe('你好谢谢');
    expect(join('', 'x')).toBe('x');
  });
});

describe('groupTurns', () => {
  const line = (track: 'mic' | 'system', start_ms: number, text: string, voice: number | null = null): RuleLine => ({
    track,
    start_ms,
    end_ms: start_ms + 1000,
    text,
    voice,
  });

  it('closes a turn after three sentences, and the next keeps its voice', () => {
    const turns = groupTurns(
      [line('system', 0, 'One.', 4), line('system', 1500, 'Two.', 4), line('system', 3000, 'Three.', 4), line('system', 4500, 'Four.')],
      true,
    );
    expect(turns.map((t) => [t.text, t.voice])).toEqual([
      ['One. Two. Three.', 4],
      ['Four.', 4],
    ]);
  });

  it('per track, an unfinished sentence reads whole across the other track', () => {
    const lines = [line('mic', 0, 'The plan is to call'), line('system', 2000, 'Sounds good.'), line('mic', 30_000, 'Priya.')];
    expect(groupTurns(lines, true).map((t) => t.text)).toEqual(['The plan is to call Priya.', 'Sounds good.']);
    // Captions join only onto the line just before.
    expect(groupTurns(lines, false).map((t) => t.text)).toEqual(['The plan is to call', 'Sounds good.', 'Priya.']);
  });
});
