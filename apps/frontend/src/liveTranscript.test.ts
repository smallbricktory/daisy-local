// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import {
  labelLines,
  captionLabels,
  mergeFinalInto,
  regroupFinals,
  applySpeakers,
  NO_SPEAKERS,
  slotForSpan,
  speakerLabel,
  type LiveSpeakersState,
  type TurnEntry,
  type LiveTurn,
} from './liveTranscript';
import type { LiveSpeakerTrack } from './tauri';

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

const ana: LiveSpeakerTrack = {
  slot: 0,
  turns: [{ start_s: 0, end_s: 5 }],
  name: { voice_id: 'v-ana', display_name: 'Ana Okafor' },
};
const second: LiveSpeakerTrack = { slot: 1, turns: [{ start_s: 5, end_s: 10 }], name: { ordinal: 2 } };
const unnamed: LiveSpeakerTrack = { slot: 2, turns: [{ start_s: 10, end_s: 12 }] };
const spk = (system: LiveSpeakerTrack[], m: LiveSpeakerTrack[] = []): LiveSpeakersState => ({ system, mic: m });

describe('slotForSpan', () => {
  it('picks the slot with the most overlap', () => {
    expect(slotForSpan([ana, second], 3000, 9000)).toBe(1);
    expect(slotForSpan([ana, second], 1000, 6000)).toBe(0);
  });
  it('returns null when the span overlaps no turn', () => {
    expect(slotForSpan([ana, second], 20000, 21000)).toBeNull();
    expect(slotForSpan([], 0, 1000)).toBeNull();
  });
});

describe('speakerLabel', () => {
  const span = { start_ms: 1000, end_ms: 2000 };
  it('uses the display name, or Speaker <ordinal>', () => {
    expect(speakerLabel('system', span, spk([ana, second]), true)).toBe('Ana Okafor');
    expect(speakerLabel('system', { start_ms: 6000, end_ms: 7000 }, spk([ana, second]), true)).toBe('Speaker 2');
  });
  it('keeps Them with no event, an unnamed slot or an unmapped line', () => {
    expect(speakerLabel('system', span, NO_SPEAKERS, true)).toBe('Them');
    expect(speakerLabel('system', { start_ms: 10500, end_ms: 11000 }, spk([unnamed]), true)).toBe('Them');
    expect(speakerLabel('system', { start_ms: 30000, end_ms: 31000 }, spk([ana]), true)).toBe('Them');
  });
  it('reads Me on a ticked mic and names an unticked one', () => {
    expect(speakerLabel('mic', span, spk([], [ana]), true)).toBe('Me');
    expect(speakerLabel('mic', span, spk([], [ana]), false)).toBe('Ana Okafor');
    expect(speakerLabel('mic', span, NO_SPEAKERS, false)).toBeNull();
  });
});

describe('mergeFinalInto speaker break', () => {
  it('breaks on a slot change on the same track', () => {
    const s = spk([ana, second]);
    const out = mergeFinalInto([sys('hi', 1000, 2000)], sys('hello', 6000, 7000), s);
    expect(out).toHaveLength(2);
  });
  it('merges within one slot, and when no speakers are known', () => {
    const s = spk([ana, second]);
    expect(mergeFinalInto([sys('hi', 1000, 2000)], sys('there', 2500, 3500), s)).toHaveLength(1);
    expect(mergeFinalInto([sys('hi', 1000, 2000)], sys('there', 6000, 7000))).toHaveLength(1);
  });
});

describe('mergeFinalInto sentence cap', () => {
  it('closes a line at three sentences and starts the fourth on its own', () => {
    let out: TurnEntry[] = [];
    for (const [t, text] of [[0, 'One.'], [1500, 'Two.'], [3000, 'Three.'], [4500, 'Four.']] as const) {
      out = mergeFinalInto(out, sys(text, t, t + 1000));
    }
    expect(out.map((e) => (e as LiveTurn).text)).toEqual(['One. Two. Three.', 'Four.']);
  });
  it('re-paragraphs a merged line whose finals land on different slots', () => {
    const merged = mergeFinalInto([sys('hi', 1000, 2000)], sys('there', 6000, 7000));
    expect(merged).toHaveLength(1);
    expect(regroupFinals(merged, spk([ana, second]))).toHaveLength(2);
  });
});

describe('captionLabels', () => {
  const lines: TurnEntry[] = [
    sys('a', 1000, 2000),
    sys('b', 6000, 7000),
    mic('c', 7500, 8000),
    sys('d', 8500, 9000),
  ];
  it('labels a line only when it differs from the line above', () => {
    const same: TurnEntry[] = [sys('a', 1000, 2000), sys('b', 2500, 3000)];
    expect(captionLabels(same, spk([ana]), true)).toEqual(['Ana Okafor', null]);
    expect(captionLabels(lines, spk([ana, second]), true)).toEqual(['Ana Okafor', 'Speaker 2', 'Me', 'Speaker 2']);
  });
  it('relabels settled lines when a name changes', () => {
    const renamed: LiveSpeakerTrack = { ...second, name: { voice_id: 'v-b', display_name: 'Ben Ito' } };
    expect(captionLabels(lines, spk([ana, renamed]), true)[1]).toBe('Ben Ito');
  });
  it('keeps Them with no event and Me on a ticked mic', () => {
    expect(captionLabels(lines, NO_SPEAKERS, true)).toEqual(['Them', null, 'Me', 'Them']);
  });
  it('a marker resets the comparison', () => {
    const withPause: TurnEntry[] = [sys('a', 1000, 2000), pause('x'), sys('b', 2500, 3000)];
    expect(captionLabels(withPause, spk([ana]), true)).toEqual(['Ana Okafor', null, 'Ana Okafor']);
  });
});

describe('labelLines', () => {
  const names = {
    system: [
      { slot: 0, turns: [{ start_s: 0, end_s: 5 }], name: { voice_id: 'v-1', display_name: 'Ana Okafor' } },
      { slot: 2, turns: [{ start_s: 5, end_s: 12 }], name: { ordinal: 3 } },
    ],
    mic: [],
    single_local_speaker: true,
    lines: [],
  };
  const line = (track: 'mic' | 'system', start_ms: number, text: string) => ({ track, start_ms, end_ms: start_ms + 2000, text });

  it('names system lines by most overlap, joins same-label runs, and keeps the owner untagged', () => {
    const out = labelLines(
      [line('system', 0, 'One.'), line('system', 2500, 'Two.'), line('system', 6000, 'Three.'), line('mic', 9000, 'Four.')],
      names,
    );
    expect(out).toEqual([
      { label: 'Ana Okafor', side: 'them', text: 'One. Two.' },
      { label: 'Speaker 3', side: 'them', text: 'Three.' },
      { label: null, side: 'me', text: 'Four.' },
    ]);
  });

  it('reads Them for an uncovered system line, and with no overlay at all', () => {
    expect(labelLines([line('system', 20_000, 'Late.')], names)[0].label).toBe('Them');
    expect(labelLines([line('system', 0, 'Early.')], null)[0].label).toBe('Them');
  });
});

describe('regroupFinals', () => {
  it('splits a line merged before speakers arrived once its finals map to two slots', () => {
    let finals: TurnEntry[] = [];
    finals = mergeFinalInto(finals, sys('hi all', 1000, 2000));
    finals = mergeFinalInto(finals, sys('thanks Ana', 6000, 7000));
    expect(finals).toHaveLength(1);
    const out = regroupFinals(finals, spk([ana, second]));
    expect(out).toHaveLength(2);
    expect(captionLabels(out, spk([ana, second]), true)).toEqual(['Ana Okafor', 'Speaker 2']);
    expect((out[1] as LiveTurn).text).toBe('thanks Ana');
  });
  it('keeps a line whose finals stay on one slot, by reference', () => {
    const finals = mergeFinalInto([sys('a', 1000, 2000)], sys('b', 2500, 3000));
    expect(regroupFinals(finals, spk([ana, second]))).toBe(finals);
  });
});

describe('applySpeakers', () => {
  const name = { ordinal: 1 };
  it('extends closed turns, replaces the open one, and resets on a full event', () => {
    const p = (full: boolean, turns: [number, number][], open: [number, number][]) => ({
      session_id: 's', track: 'system' as const, full,
      tracks: [{ slot: 0, name, turns: turns.map(([a, b]) => ({ start_s: a, end_s: b })), open: open.map(([a, b]) => ({ start_s: a, end_s: b })) }],
    });
    let st = applySpeakers(new Map(), [], p(true, [[0, 1]], [[2, 3]]));
    expect(st.shown[0].turns).toEqual([{ start_s: 0, end_s: 1 }, { start_s: 2, end_s: 3 }]);
    st = applySpeakers(st.closed, st.shown, p(false, [[2, 4]], []));
    expect(st.shown[0].turns).toEqual([{ start_s: 0, end_s: 1 }, { start_s: 2, end_s: 4 }]);
    st = applySpeakers(st.closed, st.shown, p(true, [[0, 1], [2, 4]], [[5, 6]]));
    expect(st.shown[0].turns).toHaveLength(3);
  });
});
