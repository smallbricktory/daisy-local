// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { namedTurns } from './liveTranscript';
import type { LiveSpeakerTrack } from './tauri';

describe('namedTurns (stopped, undiarized session)', () => {
  const ana: LiveSpeakerTrack = {
    slot: 0,
    turns: [{ start_s: 0, end_s: 10 }],
    name: { voice_id: 'v-ana', display_name: 'Ana Okafor' },
  };
  const bram: LiveSpeakerTrack = { slot: 3, turns: [{ start_s: 10, end_s: 20 }], name: { ordinal: 2 } };
  const line = (track: 'mic' | 'system', s: number, e: number, text: string) => ({
    track,
    start_ms: s,
    end_ms: e,
    text,
  });

  it('names lines by most overlap on the session clock and joins neighbours', () => {
    const out = namedTurns({
      system: [ana, bram],
      mic: [],
      single_local_speaker: true,
      lines: [
        line('system', 1_000, 4_000, 'Welcome.'),
        line('system', 4_500, 9_000, 'Let us begin.'),
        line('mic', 9_500, 11_000, 'Thanks.'),
        line('system', 12_000, 15_000, 'Agreed.'),
      ],
    });
    expect(out).toEqual([
      { ts: '00:00:01', who: 'Ana Okafor', side: 'them', text: 'Welcome. Let us begin.' },
      { ts: '00:00:09', who: 'Me', side: 'me', text: 'Thanks.' },
      { ts: '00:00:12', who: 'Speaker 2', side: 'them', text: 'Agreed.' },
    ]);
  });

  it('reads Them where no turn covers a system line, and the owner voice as Me', () => {
    const me: LiveSpeakerTrack = {
      slot: 1,
      turns: [{ start_s: 30, end_s: 40 }],
      name: { voice_id: 'v-me', display_name: 'Me' },
    };
    const out = namedTurns({
      system: [ana, me],
      mic: [],
      single_local_speaker: true,
      lines: [line('system', 50_000, 52_000, 'Hello?'), line('system', 31_000, 33_000, 'Mine.')],
    });
    expect(out.map((t) => [t.ts, t.who, t.side])).toEqual([
      ['00:00:31', 'Me', 'me'],
      ['00:00:50', 'Them', 'them'],
    ]);
  });

  it('labels a room mic by its own slots, hour-long timestamps included', () => {
    const room: LiveSpeakerTrack = {
      slot: 0,
      turns: [{ start_s: 3_700, end_s: 3_720 }],
      name: { ordinal: 1 },
    };
    const out = namedTurns({
      system: [],
      mic: [room],
      single_local_speaker: false,
      lines: [line('mic', 3_701_000, 3_705_000, 'In the room.')],
    });
    expect(out).toEqual([{ ts: '01:01:41', who: 'Speaker 1', side: 'them', text: 'In the room.' }]);
  });
});
