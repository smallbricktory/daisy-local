// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Speaker names above live captions, driven by the `live-speakers` event.
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { nav } from '../pages/shell.ts';
import { record } from '../pages/record.ts';

const final = (track: 'system' | 'mic', start_ms: number, end_ms: number, text: string) =>
  emitAppEvent('transcript:segment', {
    track,
    kind: { type: 'final', start_ms, end_ms, text, confidence: null },
  });

const speakers = (secondName: { voice_id: string; display_name: string } | { ordinal: number }) =>
  emitAppEvent('live-speakers', {
    // The world's recording session.
    session_id: 's-live',
    track: 'system',
    full: true,
    tracks: [
      {
        slot: 0,
        turns: [{ start_s: 0, end_s: 5 }],
        open: [],
        name: { voice_id: '8d1e3a40-0000-4000-8000-00000000000b', display_name: 'Ana Okafor' },
      },
      { slot: 1, turns: [], open: [{ start_s: 5, end_s: 10 }], name: secondName },
    ],
  });

const lineCount = async () => (await record.lines().getElements()).length;
const expectWho = async (want: string) => {
  await browser.waitUntil(async () => (await whoTexts()).join('|') === want).catch(() => {});
  expect((await whoTexts()).join('|')).toBe(want);
};
const whoTexts = async () => {
  const out: string[] = [];
  for (const el of await record.who().getElements()) out.push(await el.getProperty('textContent') as string);
  return out;
};

describe('live speaker names', function () {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nav.go('record');
    await record.begin('Names e2e');
    await record.dismissLiveNote();
  });

  after(async function () {
    if (isNative()) return;
    await record.discardIt();
  });

  it('keeps "Them" while no live-speakers event has arrived', async () => {
    await final('system', 1000, 2000, 'Welcome everyone');
    await browser.waitUntil(async () => (await lineCount()) === 1);
    expect(await whoTexts()).toEqual(['Them']);
  });

  it('names settled lines, and reads "Me" on the ticked mic', async () => {
    await speakers({ ordinal: 2 });
    await final('system', 6000, 7000, 'Thanks Ana');
    await final('mic', 8000, 8500, 'Happy to be here');
    await browser.waitUntil(async () => (await lineCount()) === 3);
    await expectWho('Ana Okafor|Speaker 2|Me');
  });

  it('relabels settled lines when a name changes', async () => {
    await speakers({ voice_id: '8d1e3a40-0000-4000-8000-00000000000c', display_name: 'Ben Ito' });
    await expectWho('Ana Okafor|Ben Ito|Me');
  });

  it('ignores an event for another session', async () => {
    await emitAppEvent('live-speakers', {
      session_id: 's-other', track: 'system', full: true,
      tracks: [{ slot: 0, turns: [{ start_s: 10, end_s: 12 }], open: [], name: { ordinal: 7 } }],
    });
    await final('system', 11000, 11500, 'Noted');
    await browser.waitUntil(async () => (await lineCount()) === 4);
    await expectWho('Ana Okafor|Ben Ito|Me|Them');
  });
});

describe('live speaker names on a recording started elsewhere', function () {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nav.go('record');
    await expect(record.start()).toBeDisplayed();
  });

  after(async function () {
    if (isNative()) return;
    await record.discardIt();
  });

  it('labels the mic as the recording was started: a room mic names its speakers', async () => {
    // A recording started outside this page, with several voices at the mic.
    await emitAppEvent('recording:snapshot', {
      state: 'recording', session_id: 's-room', session_root: '/tmp/s-room',
      started_at_unix_seconds: 0, recorded_ms: 0, clock_running_since_unix_ms: Date.now(),
      live_mode_label: 'Nemotron Speech Streaming 0.6B (int8)', single_local_speaker: false,
    });
    await expect(record.pause()).toBeDisplayed();
    await record.dismissLiveNote();
    await emitAppEvent('live-speakers', {
      session_id: 's-room',
      track: 'mic',
      full: true,
      tracks: [{
        slot: 0, turns: [{ start_s: 0, end_s: 5 }], open: [],
        name: { voice_id: '8d1e3a40-0000-4000-8000-00000000000b', display_name: 'Ana Okafor' },
      }],
    });
    await final('mic', 1000, 2000, 'Morning all');
    await browser.waitUntil(async () => (await lineCount()) === 1);
    await expectWho('Ana Okafor');
  });
});
