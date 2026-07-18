import { describe, it, expect } from 'vitest';
import { mergeFinalInto, isMicBleedOfSystem, type TurnEntry, type LiveTurn } from './liveTranscript';

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

describe('isMicBleedOfSystem (live speaker-bleed suppression)', () => {
  it('catches reworded long echoes by word containment', () => {
    const recent = [{ words: 'that and that will be the time that i will be talking to you about money'.split(' '), start_ms: 10_000 }];
    expect(isMicBleedOfSystem(mic('That. That is the time that I will be talking to you about', 11_000, 13_000), recent)).toBe(true);
    expect(isMicBleedOfSystem(mic('we should circle back on the budget spreadsheet tomorrow', 12_000, 13_500), recent)).toBe(false);
  });

  it('tolerates mangled words in order and respects the asymmetric window', () => {
    const recent = [{ words: 'we should share it with the suppliers next week'.split(' '), start_ms: 50_000 }];
    // Mangled plural + dropped word, trailing the source → echo.
    expect(isMicBleedOfSystem(mic('share with the supplier next', 51_000, 52_000), recent)).toBe(true);
    // Identical words but the mic spoke 5s BEFORE the remote → kept.
    expect(isMicBleedOfSystem(mic('share it with the suppliers', 44_000, 45_000), recent)).toBe(false);
    // 1.5s lead = decode jitter → still echo.
    expect(isMicBleedOfSystem(mic('share it with the suppliers', 48_500, 49_000), recent)).toBe(true);
  });

  it('drops short verbatim fragments of a nearby remote turn, keeps free-standing shorts', () => {
    const recent = [{ words: 'all that we can share with suppliers about'.split(' '), start_ms: 99_000 }];
    expect(isMicBleedOfSystem(mic('Share with', 99_500, 100_000), recent)).toBe(true);
    expect(isMicBleedOfSystem(mic('Agreed.', 100_200, 100_500), recent)).toBe(false);
    expect(isMicBleedOfSystem(mic('share with', 120_000, 120_400), recent)).toBe(false);
  });

  const sysFinal = (text: string, start_ms: number) => ({ words: norm(text), start_ms });
  function norm(t: string): string[] {
    return t.toLowerCase().replace(/[^a-z0-9\s]/g, ' ').split(/\s+/).filter(Boolean);
  }

  it('flags a mic final that echoes a recent system final', () => {
    const recent = [sysFinal('Danny, any luck with the chase messages', 1000)];
    expect(isMicBleedOfSystem(mic('Danny any luck with the chase', 1200, 3000), recent)).toBe(true);
  });

  it('keeps the user’s own speech (no system twin)', () => {
    const recent = [sysFinal('so I think we have everyone here', 1000)];
    expect(isMicBleedOfSystem(mic('I need to take a look at that', 1200, 3000), recent)).toBe(false);
  });

  it('short finals: verbatim fragments are bleed, reordered words are not', () => {
    const recent = [sysFinal('yeah okay sure', 1000)];
    // In-order fragment of the remote text → substring echo, dropped.
    expect(isMicBleedOfSystem(mic('yeah okay', 1100, 1500), recent)).toBe(true);
    // Same words, different order → not a verbatim echo → kept.
    expect(isMicBleedOfSystem(mic('okay yeah', 1100, 1500), recent)).toBe(false);
  });

  it('does not match outside the trailing time window', () => {
    const recent = [sysFinal('the quarterly numbers look strong this period', 1000)];
    expect(isMicBleedOfSystem(mic('the quarterly numbers look strong this period', 99000, 101000), recent)).toBe(false);
  });

  it('catches a fragment contained in a longer system final', () => {
    const recent = [sysFinal('and I think Catherine will join us shortly', 1000)];
    expect(isMicBleedOfSystem(mic('I think Catherine will join', 1500, 3000), recent)).toBe(true);
  });
});
