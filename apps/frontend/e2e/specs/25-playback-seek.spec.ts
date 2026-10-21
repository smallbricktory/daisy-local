// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect } from '@wdio/globals';
import { isNative, sessionDir } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { CELL_SECONDS, cellIsNoise, synthMeetingOpus } from '../fixtures/opus.ts';
import { nav, toast } from '../pages/shell.ts';
import { library, newMeeting } from '../pages/library.ts';
import { player } from '../pages/session.ts';

/** Two notes given a synthetic meeting.opus each: 20 minutes (2 MB) and
 *  100 minutes (9.9 MB). */
const SHORT = { title: 'Playback 20 min', seconds: 20 * 60 };
const LONG = { title: 'Playback 100 min', seconds: 100 * 60 };
const PLAY_MS = 4_000;
/** Readings this soon after play() can still carry audio from before the seek. */
const SETTLE_MS = 200;
/** A level above this is noise; the silent cells decode to zero. */
const NOISE_RMS = 0.01;
/** Share of level readings the best offset must explain. */
const MIN_MATCH = 0.75;

let shortId = '';
let longId = '';

async function openWithAudio(id: string, other: string) {
  // Re-opening the row makes the pane fetch the audio the test just wrote.
  await library.openRow(other);
  await library.openRow(id);
  await player.waitReady(60_000);
  expect(await player.error()).toBeNull();
}

/** Where in the file the levels were heard, relative to `target`: the shift
 *  of the fixture's cell pattern, searched over the whole file, that best
 *  explains them. */
function heardAt(target: number, duration: number, levels: Array<[number, number]>): { shift: number; match: number } {
  const heard = levels.filter(([ms]) => ms >= SETTLE_MS).map(([ms, rms]) => [ms / 1000, rms > NOISE_RMS ? 1 : 0] as const);
  let best = { shift: Number.NaN, match: 0 };
  for (let shift = -target - 1; shift < duration - target; shift += 0.02) {
    let agree = 0;
    for (const [e, bit] of heard) {
      const pos = target + shift + e;
      // Outside the file the player is silent.
      const want = pos >= 0 && pos < duration && cellIsNoise(Math.floor(pos / CELL_SECONDS)) ? 1 : 0;
      if (want === bit) agree++;
    }
    const match = agree / heard.length;
    if (match > best.match) best = { shift, match };
  }
  return best;
}

/** Seeks to each target and checks the audio heard comes from there: the
 *  played noise pattern matches the file within 1 s of the target, and
 *  `currentTime` agrees within 1.5 s. */
async function seekBattery(targets: number[], duration: number) {
  const results = [];
  for (const t of targets) {
    const started = Date.now();
    const r = await player.seek(t, PLAY_MS);
    const noisy = r.levels.filter(([, rms]) => rms > NOISE_RMS).length;
    results.push({ t, ...r, ms: Date.now() - started - r.playedMs, noisy, ...heardAt(t, duration, r.levels) });
  }
  for (const r of results) {
    console.log(`[playback] ${r.t}s -> ${r.event} at ${r.seeked.toFixed(2)}s (+${r.ms.toFixed(0)} ms), heard at ${(r.t + r.shift).toFixed(2)}s (shift ${r.shift.toFixed(2)}s, match ${r.match.toFixed(2)}, ${r.noisy}/${r.levels.length} noisy), currentTime after ${(r.playedMs / 1000).toFixed(2)}s of play ${r.played.toFixed(2)}s`);
  }
  for (const r of results) {
    expect(r.event).toBe('seeked');
    expect(Math.abs(r.seeked - r.t)).toBeLessThan(1);
    expect(r.noisy).toBeGreaterThan(0);
    expect(r.match).toBeGreaterThanOrEqual(MIN_MATCH);
    expect(Math.abs(r.shift)).toBeLessThan(1);
    expect(Math.abs(r.played - (r.t + r.playedMs / 1000))).toBeLessThan(1.5);
  }
}

/** A 1 s 16 kHz mono 440 Hz WAV, as the speaker-sample clips are laid out. */
function toneWav(): Buffer {
  const rate = 16_000;
  const n = rate;
  const b = Buffer.alloc(44 + n * 2);
  b.write('RIFF', 0); b.writeUInt32LE(36 + n * 2, 4); b.write('WAVE', 8);
  b.write('fmt ', 12); b.writeUInt32LE(16, 16); b.writeUInt16LE(1, 20); b.writeUInt16LE(1, 22);
  b.writeUInt32LE(rate, 24); b.writeUInt32LE(rate * 2, 28); b.writeUInt16LE(2, 32); b.writeUInt16LE(16, 34);
  b.write('data', 36); b.writeUInt32LE(n * 2, 40);
  for (let i = 0; i < n; i++) b.writeInt16LE(Math.round(8000 * Math.sin((2 * Math.PI * 440 * i) / rate)), 44 + i * 2);
  return b;
}

// The Linux player reads meeting.opus from disk and seeks within the file;
// the other webviews play it from memory and run the browser-side checks.
describe('session playback on Linux', () => {
  before(async function () {
    if (!isNative() || process.platform !== 'linux') {
      console.log('[playback] skipped: these checks run in the native Linux app');
      this.skip();
    }
    await bootWorld();
    await nav.go('library');
    await newMeeting.createNote(SHORT.title, 'Synthetic audio');
    await newMeeting.createNote(LONG.title, 'Synthetic audio');
    shortId = await library.rowIdByTitle(SHORT.title);
    longId = await library.rowIdByTitle(LONG.title);
    writeFileSync(join(sessionDir(shortId), 'meeting.opus'), synthMeetingOpus(SHORT.seconds));
    writeFileSync(join(sessionDir(longId), 'meeting.opus'), synthMeetingOpus(LONG.seconds));
    if (await toast.byId('session-status').isExisting()) await toast.dismissById('session-status');
  });

  after(() => {
    if (!isNative() || process.platform !== 'linux') return;
    for (const id of [shortId, longId]) {
      if (id) rmSync(join(sessionDir(id), 'meeting.opus'), { force: true });
    }
  });

  it('the profile path holds an @ and spaces', () => {
    const profile = process.env.DAISY_PROFILE_DIR ?? '';
    expect(profile).toContain('@');
    expect(profile).toContain(' ');
  });

  it('a 20-minute recording plays from every seek target', async function () {
    this.timeout(240_000);
    await openWithAudio(shortId, longId);
    expect(await player.src()).toMatch(/^file:\/\/.*%40.*\/meeting\.opus$/);
    expect(Math.abs((await player.duration()) - SHORT.seconds)).toBeLessThan(1);
    await seekBattery([309, 366, 0, 729, 1177, 60, 600], SHORT.seconds);
  });

  it('a 100-minute recording plays from every seek target', async function () {
    this.timeout(300_000);
    await openWithAudio(longId, shortId);
    expect(Math.abs((await player.duration()) - LONG.seconds)).toBeLessThan(1);
    await seekBattery([5_700, 300, 3_600, 5_990, 14, 4_000], LONG.seconds);
  });

  it('a WAV clip plays from a data: URL, as speaker samples do', async () => {
    const url = `data:audio/wav;base64,${toneWav().toString('base64')}`;
    const r = await browser.execute(async (src: string) => {
      const a = new Audio();
      a.muted = true;
      const loaded = await new Promise<string>((done) => {
        a.onloadedmetadata = () => done('loaded');
        a.onerror = () => done(`error ${a.error?.code}`);
        setTimeout(() => done('timeout'), 10_000);
        a.src = src;
      });
      if (loaded !== 'loaded') return { loaded, duration: Number.NaN, played: 0 };
      await a.play();
      await new Promise((ok) => setTimeout(ok, 800));
      a.pause();
      return { loaded, duration: a.duration, played: a.currentTime };
    }, url);
    console.log(`[playback] data: clip ${JSON.stringify(r)}`);
    expect(r.loaded).toBe('loaded');
    expect(Math.abs(r.duration - 1)).toBeLessThan(0.05);
    expect(r.played).toBeGreaterThan(0.3);
  });
});
