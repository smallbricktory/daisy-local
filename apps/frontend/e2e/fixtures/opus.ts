// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Synthetic meeting.opus: an Ogg-Opus stream laid out as a finished recording
// (stereo, pre-skip 0, 20 ms packets, 24 kbps) whose packets are CELT frames
// of seeded pseudo-random bytes. Decoders play them as noise. The stream is
// cut into 100 ms cells, each noise or digital silence by a hash of its
// index.

/** Bytes per 20 ms packet: 24 kbps. */
const PACKET_BYTES = 60;
/** TOC: CELT fullband 20 ms (config 31), stereo, one frame. */
const TOC = (31 << 3) | 0x04;
const PACKETS_PER_PAGE = 50;
const SAMPLES_PER_PACKET = 960;
const SERIAL = 0x0da15e01;
/** Length of one noise-or-silence cell, in seconds. */
export const CELL_SECONDS = 0.1;
const PACKETS_PER_CELL = 5;
/** A CELT 20 ms frame the decoder plays as digital silence. */
const SILENT_FRAME = [0xff, 0xfe];

/** Whether cell `i` holds noise (else silence). */
export function cellIsNoise(i: number): boolean {
  let x = Math.imul(i + 1, 0x9e3779b1) >>> 0;
  x ^= x >>> 15;
  x = Math.imul(x, 0x85ebca6b) >>> 0;
  x ^= x >>> 13;
  return (x & 1) === 1;
}

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let r = i << 24;
    for (let k = 0; k < 8; k++) r = r & 0x80000000 ? (r << 1) ^ 0x04c11db7 : r << 1;
    t[i] = r >>> 0;
  }
  return t;
})();

function oggCrc(b: Uint8Array): number {
  let crc = 0;
  for (const x of b) crc = ((crc << 8) ^ CRC_TABLE[((crc >>> 24) ^ x) & 0xff]) >>> 0;
  return crc;
}

function page(packets: Uint8Array[], granule: bigint, seq: number, flags: number): Uint8Array {
  const lacing: number[] = [];
  for (const p of packets) {
    let n = p.length;
    while (n >= 255) {
      lacing.push(255);
      n -= 255;
    }
    lacing.push(n);
  }
  const bodyLen = packets.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(27 + lacing.length + bodyLen);
  const dv = new DataView(out.buffer);
  out.set([0x4f, 0x67, 0x67, 0x53, 0, flags]);
  dv.setBigUint64(6, granule, true);
  dv.setUint32(14, SERIAL, true);
  dv.setUint32(18, seq, true);
  out[26] = lacing.length;
  out.set(lacing, 27);
  let at = 27 + lacing.length;
  for (const p of packets) {
    out.set(p, at);
    at += p.length;
  }
  dv.setUint32(22, oggCrc(out), true);
  return out;
}

/** An Ogg-Opus file of `seconds` of stereo noise. */
export function synthMeetingOpus(seconds: number): Uint8Array {
  const head = new Uint8Array(19);
  head.set([...'OpusHead'].map((c) => c.charCodeAt(0)));
  const hv = new DataView(head.buffer);
  head[8] = 1;
  head[9] = 2;
  hv.setUint16(10, 0, true);
  hv.setUint32(12, 16_000, true);
  const vendor = [...'Daisy e2e'].map((c) => c.charCodeAt(0));
  const tags = new Uint8Array(8 + 4 + vendor.length + 4);
  tags.set([...'OpusTags'].map((c) => c.charCodeAt(0)));
  new DataView(tags.buffer).setUint32(8, vendor.length, true);
  tags.set(vendor, 12);

  const pages: Uint8Array[] = [page([head], 0n, 0, 0x02), page([tags], 0n, 1, 0)];
  const total = Math.round((seconds * 1000) / 20);
  let rng = 0x2545f491;
  let granule = 0n;
  for (let first = 0; first < total; first += PACKETS_PER_PAGE) {
    const count = Math.min(PACKETS_PER_PAGE, total - first);
    const packets: Uint8Array[] = [];
    for (let k = 0; k < count; k++) {
      if (!cellIsNoise(Math.floor((first + k) / PACKETS_PER_CELL))) {
        packets.push(new Uint8Array([TOC, ...SILENT_FRAME]));
        continue;
      }
      const p = new Uint8Array(PACKET_BYTES);
      p[0] = TOC;
      for (let i = 1; i < PACKET_BYTES; i++) {
        rng ^= rng << 13;
        rng ^= rng >>> 17;
        rng ^= rng << 5;
        p[i] = rng & 0xff;
      }
      packets.push(p);
    }
    granule += BigInt(count * SAMPLES_PER_PACKET);
    const last = first + count >= total;
    pages.push(page(packets, granule, pages.length, last ? 0x04 : 0));
  }
  const out = new Uint8Array(pages.reduce((a, p) => a + p.length, 0));
  let at = 0;
  for (const p of pages) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}
