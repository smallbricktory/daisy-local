// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect } from '@wdio/globals';
import { isNative, sessionDir } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { CELL_SECONDS, PACKET_SECONDS, cellIsNoise, oggPacketSizes, synthMeetingOpus } from '../fixtures/opus.ts';
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
/** Whether the analyser listens through a new element loading the player's
 *  src for each target. The audio checks then cover the file the player
 *  plays; the player's own element is checked through its seek and
 *  `currentTime` alone. */
const HEAR_FRESH = process.platform === 'darwin';
/** Whether the position check reads the player's decoded byte count in
 *  place of its signal level. The fixture's packets are 60 bytes in a noise
 *  cell and 3 in a silent one, so the count's growth during the play traces
 *  the cell pattern from where the decoder reads. */
const COUNT_BYTES = process.platform === 'win32';
/** Share of level or byte-count readings the best offset must explain. */
const MIN_MATCH = 0.75;
/** A byte-count reading agrees with an offset when it lies this close to
 *  the file's packet sizes summed from there: half a noise cell. */
const BYTE_TOL = 150;
/** Bounds on how far ahead of the played position the decoder reads, in
 *  seconds. */
const LEAD_MIN = -0.5;
const LEAD_MAX = 2;

let shortId = '';
let longId = '';
/** Running sums of each file's packet sizes: entry k is the bytes of
 *  packets 0..k-1. */
let shortBytes: number[] = [];
let longBytes: number[] = [];

function runningSums(sizes: number[]): number[] {
  const out = [0];
  for (const s of sizes) out.push(out[out.length - 1] + s);
  return out;
}

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

/** Where in the file the decoder read during the play, relative to
 *  `target`: the shift of the file's packet sizes (`sums`, running), searched
 *  over the whole file in packet steps, that best explains the growth of
 *  the decoded byte count since the first reading. */
function decodedAt(target: number, sums: number[], decoded: Array<[number, number]>): { shift: number; match: number } {
  const packets = sums.length - 1;
  const at = (s: number) => sums[Math.min(packets, Math.max(0, Math.floor(s / PACKET_SECONDS)))];
  let best = { shift: Number.NaN, match: 0 };
  if (decoded.length < 2) return best;
  const [ms0, bytes0] = decoded[0];
  const first = Math.floor((-target - 1) / PACKET_SECONDS);
  const last = Math.ceil((packets * PACKET_SECONDS - target) / PACKET_SECONDS);
  for (let k = first; k < last; k++) {
    const shift = k * PACKET_SECONDS;
    const base = at(target + shift + ms0 / 1000);
    let agree = 0;
    for (const [ms, bytes] of decoded) {
      if (Math.abs(bytes - bytes0 - (at(target + shift + ms / 1000) - base)) <= BYTE_TOL) agree++;
    }
    const match = agree / decoded.length;
    if (match > best.match) best = { shift, match };
  }
  return best;
}

/** Seeks to each target and checks the audio comes from there: the played
 *  noise pattern (or, with COUNT_BYTES, the decoded byte count's growth)
 *  matches the file near the target, and `currentTime` agrees within
 *  1.5 s. */
async function seekBattery(targets: number[], duration: number, sums: number[]) {
  const results = [];
  for (const t of targets) {
    const started = Date.now();
    const r = await player.seek(t, PLAY_MS, !HEAR_FRESH);
    const ms = Date.now() - started - r.playedMs;
    const levels = HEAR_FRESH ? await player.hear(t, PLAY_MS) : r.levels;
    const noisy = levels.filter(([, rms]) => rms > NOISE_RMS).length;
    const grew = r.decoded.length > 0 ? r.decoded[r.decoded.length - 1][1] - r.decoded[0][1] : Number.NaN;
    const read = COUNT_BYTES ? decodedAt(t, sums, r.decoded) : { shift: Number.NaN, match: 0 };
    results.push({ t, ...r, levels, ms, noisy, ...heardAt(t, duration, levels), grew, read });
  }
  for (const r of results) {
    const where = COUNT_BYTES
      ? `decoder read from ${(r.t + r.read.shift).toFixed(2)}s (lead ${r.read.shift.toFixed(2)}s, match ${r.read.match.toFixed(2)}, ${r.grew} bytes over ${r.decoded.length} readings)`
      : `heard at ${(r.t + r.shift).toFixed(2)}s (shift ${r.shift.toFixed(2)}s, match ${r.match.toFixed(2)}, ${r.noisy}/${r.levels.length} noisy)`;
    console.log(`[playback] ${r.t}s -> ${r.event} at ${r.seeked.toFixed(2)}s (+${r.ms.toFixed(0)} ms), ${where}, currentTime after ${(r.playedMs / 1000).toFixed(2)}s of play ${r.played.toFixed(2)}s`);
  }
  for (const r of results) {
    expect(r.event).toBe('seeked');
    expect(Math.abs(r.seeked - r.t)).toBeLessThan(1);
    if (COUNT_BYTES) {
      expect(r.grew).toBeGreaterThan(0);
      expect(r.read.match).toBeGreaterThanOrEqual(MIN_MATCH);
      expect(r.read.shift).toBeGreaterThan(LEAD_MIN);
      expect(r.read.shift).toBeLessThan(LEAD_MAX);
    } else {
      expect(r.noisy).toBeGreaterThan(0);
      expect(r.match).toBeGreaterThanOrEqual(MIN_MATCH);
      expect(Math.abs(r.shift)).toBeLessThan(1);
    }
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

// The Linux player reads meeting.opus decrypted to a file under
// $XDG_RUNTIME_DIR/daisy-media/<pid>/ and seeks within that file; the other
// webviews play it through the daisy-media scheme.
describe('session playback', () => {
  before(async function () {
    if (!isNative()) {
      console.log('[playback] skipped: these checks run in the native app');
      this.skip();
    }
    await bootWorld();
    await nav.go('library');
    await newMeeting.createNote(SHORT.title, 'Synthetic audio');
    await newMeeting.createNote(LONG.title, 'Synthetic audio');
    shortId = await library.rowIdByTitle(SHORT.title);
    longId = await library.rowIdByTitle(LONG.title);
    const shortFile = synthMeetingOpus(SHORT.seconds);
    const longFile = synthMeetingOpus(LONG.seconds);
    shortBytes = runningSums(oggPacketSizes(shortFile));
    longBytes = runningSums(oggPacketSizes(longFile));
    writeFileSync(join(sessionDir(shortId), 'meeting.opus'), shortFile);
    writeFileSync(join(sessionDir(longId), 'meeting.opus'), longFile);
    if (await toast.byId('session-status').isExisting()) await toast.dismissById('session-status');
  });

  after(() => {
    if (!isNative()) return;
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
    const src = await player.src();
    expect(src).toMatch(
      process.platform === 'linux'
        ? /^file:\/\/.*\/daisy-media\/\d+\/[0-9a-f]{32}\.opus$/
        : process.platform === 'win32'
          ? /^http:\/\/daisy-media\.localhost\//
          : /^daisy-media:\/\/localhost\//,
    );
    // The Linux file name keeps the session id off the tmpfs listing; the
    // daisy-media route names the session.
    if (process.platform === 'linux') expect(src).not.toContain(shortId);
    expect(Math.abs((await player.duration()) - SHORT.seconds)).toBeLessThan(1);
    await seekBattery([309, 366, 0, 729, 1177, 60, 600], SHORT.seconds, shortBytes);
  });

  it('a 100-minute recording plays from every seek target', async function () {
    this.timeout(300_000);
    await openWithAudio(longId, shortId);
    expect(Math.abs((await player.duration()) - LONG.seconds)).toBeLessThan(1);
    await seekBattery([5_700, 300, 3_600, 5_990, 14, 4_000], LONG.seconds, longBytes);
  });

  // Speaker samples play from a data: URL in the Linux webview and from a
  // Blob URL elsewhere (src/lib/audioSrc.ts); the CSP allows each where it
  // is used.
  it('a WAV clip plays from the URL speaker samples use', async () => {
    const b64 = toneWav().toString('base64');
    const r = await browser.execute(async (bytes: string, asData: boolean) => {
      let src = `data:audio/wav;base64,${bytes}`;
      if (!asData) {
        const bin = atob(bytes);
        const u8 = new Uint8Array(bin.length);
        for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
        src = URL.createObjectURL(new Blob([u8], { type: 'audio/wav' }));
      }
      const a = new Audio();
      a.muted = true;
      const loaded = await new Promise<string>((done) => {
        a.onloadedmetadata = () => done('loaded');
        a.onerror = () => done(`error ${a.error?.code}`);
        setTimeout(() => done('timeout'), 10_000);
        a.src = src;
      });
      if (loaded !== 'loaded') return { scheme: src.slice(0, 5), loaded, duration: Number.NaN, played: 0 };
      await a.play();
      await new Promise((ok) => setTimeout(ok, 800));
      a.pause();
      return { scheme: src.slice(0, 5), loaded, duration: a.duration, played: a.currentTime };
    }, b64, process.platform === 'linux');
    console.log(`[playback] clip ${JSON.stringify(r)}`);
    expect(r.loaded).toBe('loaded');
    expect(Math.abs(r.duration - 1)).toBeLessThan(0.05);
    expect(r.played).toBeGreaterThan(0.3);
  });
});
