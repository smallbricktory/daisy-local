# diagnostics/

Analysis and measurement tools, built as cargo examples
(`[[example]] path = ...`). They are not part of any release artifact.

Grouped by what they measure. The `-p <crate>` argument is the crate that
supplies the code under test:

```sh
cargo run --release -p <crate> --example <name> -- <args>
```

## audio/ — capture, echo cancellation, denoise

| example | crate | what it answers |
|---|---|---|
| `wasapi_smoke` | audio-engine | Does WASAPI capture open and deliver frames on this Windows machine? |
| `wasapi_dual_probe` | audio-engine | Do mic + loopback capture concurrently, and at what relative clock? |
| `fs_probe` | audio-engine | Which file operation of a recording start or a model load does this drive reject, and with what OS error? |
| `live_aec_replay` | recording | Replays a session's tracks through the live AEC bridge — the A/B rig for cancellation quality and reference underruns. |
| `probe_dfn` | denoise | DFN3 noise suppression over one WAV, to hear what it does. |

`live_aec_replay` caveat: a replay does not reproduce the original session's
adaptive-gain state (auto-gain starts cold), so its absolute levels are not
comparable to the live run. Compare variants within one replay only.

## asr/ — whisper speed and accuracy

| example | crate | what it answers |
|---|---|---|
| `bench` | providers-local | Quick local-whisper benchmark on one WAV; downloads `ggml-base.en.bin` into `./bench-cache/` when no model path is given. |
| `whisper_bench` | providers-local | Full harness: sweeps thread counts (`BENCH_THREADS`), reports full-file and streaming-window xRT. |
| `whisper_text` | providers-local | Dumps one model's plain transcript, so two models can be diffed for accuracy delta. |
| `clip_replay` | tauri-app | Replays WAVs through the recording's live pipeline and writes `live_transcript.jsonl` + `telemetry.jsonl` for `scripts/clip-test-analyze.py` (see `docs/clip-test-telemetry.md`). |

## speakers/ — diarization and voiceprints

| example | crate | what it answers |
|---|---|---|
| `diag_diarize` | voiceprints | The big one: embeddings, cluster assignments, silhouette, and pairwise-cosine distributions for a session. |
| `denoise_diar_ab` | voiceprints | Does DFN3 denoise change diarization? Embeds the same mic-track segments from `mic_dn` (denoised) vs `mic_aec` (echo-cancelled fallback) and reports per-segment drift, cluster count, silhouette, assignment agreement. |
| `probe_vp` | voiceprints | Embeds a WAV and prints its voiceprint vector. |
| `probe_diarize_mic` | voiceprints | Diarizes the mic track alone — the many-people-on-one-in-person-mic case. |
| `nemotron3_diar_bench` | providers-local | Real-time factor, memory and ASR interference of one Nemotron 3 Diarization stream per preset. |

```sh
DAISY_VOICEPRINT_DIR=$PWD/models/voiceprints \
  cargo run --release -p voiceprints --example denoise_diar_ab -- <session_dir> [known_k]
```

## transcript/ — dedup and echo bleed

| example | crate | what it answers |
|---|---|---|
| `echo_probe` | transcript | How much of the mic transcript is an echo of the system track? |
| `dup_probe` | transcript | Duplicate/overlap detection across pooled segments. |

## Adding one

Put the file in the matching subdirectory and add the target to the owning
crate's `Cargo.toml`:

```toml
[[example]]
name = "my_probe"
path = "../../diagnostics/audio/my_probe.rs"
```

`cargo build --workspace --examples` builds it, and `cargo clippy --workspace
--all-targets` lints it.
