// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// A stopped session whose transcript has no speakers yet reads its live
// speaker names from the overlay files; finalize's speakers replace them.
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitLibraryChanged } from '../fixtures/events.ts';
import { mockInvoke, mockInvokeWith } from '../fixtures/invoke.ts';
import { nav } from '../pages/shell.ts';
import { library } from '../pages/library.ts';
import { session } from '../pages/session.ts';
import { tid } from '../pages/_shared.ts';
import type { UndiarizedNames } from '../../src/tauri.ts';

const SESSION = 's-001';

/** What an undiarized transcript.md reads like: "Them" and "Me" only. */
const UNDIARIZED_MD = [
  '[00:00:01] **Them**: Welcome everyone, shall we begin.',
  '',
  '[00:00:12] **Them**: Thanks, here is the status.',
  '',
  '[00:00:24] **Me**: Sounds good to me.',
].join('\n');

const LINES: UndiarizedNames['lines'] = [
  { track: 'system', start_ms: 1_000, end_ms: 6_000, text: 'Welcome everyone, shall we begin.' },
  { track: 'system', start_ms: 12_000, end_ms: 18_000, text: 'Thanks, here is the status.' },
  { track: 'mic', start_ms: 24_000, end_ms: 27_000, text: 'Sounds good to me.' },
];

const LIVE_NAMES: UndiarizedNames = {
  system: [
    {
      slot: 0,
      turns: [{ start_s: 0.5, end_s: 8 }],
      name: { voice_id: '8d1e3a40-0000-4000-8000-00000000000b', display_name: 'Ana Okafor' },
    },
    { slot: 2, turns: [{ start_s: 11, end_s: 20 }], name: { ordinal: 3 } },
  ],
  mic: [],
  single_local_speaker: true,
  lines: LINES,
};

const who = async (i: number): Promise<string> =>
  (await tid(`transcript-turn-${i}`).$('.who').getProperty('textContent')) as string;

describe('stopped session live names', function () {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld({ transcriptMd: UNDIARIZED_MD, liveNames: LIVE_NAMES });
    await nav.go('library');
    await library.openRow(SESSION);
    await session.open('transcript');
  });

  it('labels the undiarized transcript from the overlay', async () => {
    await browser.waitUntil(async () => (await who(0)) === 'Ana Okafor');
    expect(await who(1)).toBe('Speaker 3');
    // The mic line is the local user's: a "Me" turn carries no name tag.
    await expect(tid('transcript-turn-2')).toHaveElementClass('me');
    await expect(tid('transcript-turn-2').$('.who')).not.toBeExisting();
  });

  it('drops the overlay names once speakers exist', async () => {
    await mockInvoke('read_live_names', null);
    await mockInvokeWith('read_session', () => ({
      session_id: 's-001',
      manifest_json: {
        session_id: 's-001',
        title: 'Product sync',
        created_at_unix_seconds: 1_700_000_000,
        chunks: [{ duration_seconds: 2712 }],
        recording_kind: null,
      },
      transcript_md: [
        '[00:00:01] **Person A**: Welcome everyone, shall we begin.',
        '',
        '[00:00:12] **Person B**: Thanks, here is the status.',
        '',
        '[00:00:24] **Me**: Sounds good to me.',
      ].join('\n'),
      has_transcript: true,
      has_dedup: true,
      has_summary: true,
    }));
    await emitLibraryChanged(SESSION);
    await browser.waitUntil(async () => (await who(0)) === 'Person A');
    expect(await who(1)).toBe('Person B');
  });
});

const liveWho = async (i: number): Promise<string | null> => {
  const el = tid(`live-turn-${i}`).$('.who');
  return (await el.isExisting()) ? ((await el.getProperty('textContent')) as string) : null;
};

describe('finalizing session live names', function () {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      liveNames: {
        system: [
          {
            slot: 1,
            turns: [{ start_s: 0, end_s: 5 }],
            name: { voice_id: '8d1e3a40-0000-4000-8000-00000000000b', display_name: 'Ana Okafor' },
          },
        ],
        mic: [],
        single_local_speaker: true,
        lines: [],
      },
    });
    await nav.go('library');
    await library.openRow('s-005');
    await session.open('transcript');
  });

  it('labels the live transcript from the overlay before any transcript exists', async () => {
    await browser.waitUntil(async () => (await liveWho(0)) === 'Ana Okafor');
    await expect(tid('live-turn-1')).toHaveElementClass('me');
    expect(await liveWho(1)).toBeNull();
  });

  it('reads Them once no turn covers the line', async () => {
    await mockInvoke('read_live_names', {
      system: [{ slot: 1, turns: [{ start_s: 50, end_s: 60 }], name: { ordinal: 1 } }],
      mic: [],
      single_local_speaker: true,
      lines: [],
    });
    await nav.go('library');
    await library.openRow('s-005');
    await session.open('transcript');
    await browser.waitUntil(async () => (await liveWho(0)) === 'Them');
  });
});
