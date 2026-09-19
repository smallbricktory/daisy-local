# Ogg-Opus pre-skip is declared 0 while the encoder delays 104 samples

Found 2026-08-16 while spiking the iOS Ogg-Opus encode route in
`smbr-daisy-ios`. The defect is on this side, in
`crates/recording/src/compress.rs`. Not fixed here — recorded so the desktop
side can decide.

## Status: low priority, deferred (2026-08-16)

Scoped and deliberately not fixed. Re-evaluate when the iOS Ogg-Opus route
lands, so both encoders settle on one header contract.

Deferred because the offset reaches nothing timed (see *Impact*, traced below)
and because a correct fix is wider than the pre-skip field alone (see *If it
gets fixed*). It stays worth doing as format hygiene: it also recovers the
last 104 samples of every archive, which are dropped today.

## What was measured

`crates/audio-engine/tests/fixtures/meeting_2min.mic.wav` — 1 918 720 samples
of 16 kHz mono i16, an exact multiple of the 320-sample frame — was encoded
with `compress::encode_mono_pcm16` at default `CompressParams` and decoded back
with `compress::decode_opus`.

Sample count round-trips exactly: 1 918 720 in, 1 918 720 out.

The decoded audio is **104 samples (6.5 ms) later** than the source. Lag found
by cross-correlating a 4-second mid-file window over ±2000 samples; the
correlation peak sits at +104 with no ambiguity. After aligning at that lag,
difference-RMS relative to source-RMS is −7.3 dB, which is ordinary lossy-codec
error at 24 kbps. Without aligning it measures +2.7 dB — the offset alone
swamps the codec error, so any sample-aligned comparison against the source
reads as noise-dominated.

The same 104-sample lag appears in a file produced by an unrelated encoder
(`element-hq/swift-ogg`), which is what identifies it as libopus's algorithmic
delay rather than anything specific to this code.

## Cause

`compress.rs` writes the `OpusHead` identification header with pre-skip
hardcoded to 0:

```rust
head.extend_from_slice(b"OpusHead");
head.push(1);
head.push(channels);
head.extend_from_slice(&0u16.to_le_bytes());   // <- pre-skip
```

libopus at 16 kHz has 104 samples of algorithmic delay (RFC 7845 §4.2 expects
pre-skip to carry it so the decoder trims those samples). Declaring 0 means
nothing is trimmed and every `.opus` this project has ever written starts
6.5 ms late.

`opus_encoder_ctl(OPUS_GET_LOOKAHEAD)` returns the real value rather than
assuming 104 — it varies with sample rate and application mode.

## Impact: traced, and it reaches nothing timed (2026-08-16)

Every consumer of decoded archive audio was walked. The 6.5 ms is a constant
one-directional shift applied to a whole file at once; it never accumulates,
because a session is one decode of one file.

- `rehydrate_from_opus` (`crates/tauri-app/src/commands/pipeline.rs:108`) is
  the only path that feeds archive audio back into the pipeline. It decodes
  the archive into a single chunk, rewrites the manifest to that chunk, and
  the transcript and diarization are then regenerated **from those samples**.
  Timestamps and audio move together, so there is nothing for the shift to be
  measured against. A transcript left over from before the prune sits 6.5 ms
  off — far below segment granularity.
- `crates/tauri-app/src/commands/playback.rs:78` seeks by transcript
  timestamp. 6.5 ms is under one 20 ms Opus frame.
- `decode_opus_to_ulaw_wav_bytes` (the macOS µ-law playback path) shares that
  decode and inherits the same shift.

No path lines opus-decoded audio up against WAV-derived timestamps. So this is
a file-format correctness issue; the timing code stays as it is.

## If it gets fixed

Setting the pre-skip field alone shortens every file. Four edits have to land
together, all inside `crates/recording/src/compress.rs` (~25 lines):

1. **Pre-skip field in 48 kHz units.** RFC 7845 §5.1 defines pre-skip at
   48 kHz whatever the input rate. `Encoder::get_lookahead()` (`opus` 0.3.1,
   already a dependency) returns 104 at 16 kHz, so the header carries
   `104 * 3 = 312`. Writing 104 under-trims by two thirds.
2. **Flush the tail.** The encoder holds its lookahead, and no extra frame is
   encoded, so the last 104 samples of every archive written to date are
   dropped. One extra padding frame recovers them.
3. **Granule.** `final_granule` is `N * 3` (compress.rs:175). A spec decoder
   emits `granule - pre_skip`, so it has to become `(N + lookahead) * 3` for
   the decoded length to stay `N`.
4. **`decode_opus` honours the header** — trim `pre_skip / 3` from the head
   and truncate to granule, rather than assuming a value.

Existing archives declare pre-skip 0, and a header-honouring decoder trims 0
from them, so they keep today's behavior with no migration.

The test to add is the one this investigation used and the suite lacks:
round-trip a fixture, assert the cross-correlation peak lag is 0 and the
decoded length is exactly `N`. The current tests are 15 %-tolerance checks
that pass either way.

Check macOS playback before landing: the WebKit/CoreMedia duration and seek
fix keys on granule. macOS plays the µ-law WAV, whose duration comes from the
WAV header, so the granule change should not reach it.

## Reproducing

The iOS-side probes were throwaway and are gone. Reproducing needs only this
repo: encode a fixture with `encode_mono_pcm16`, decode with `decode_opus`,
and cross-correlate the result against the source PCM to find the peak lag.
The expected answer is +104 at 16 kHz.
