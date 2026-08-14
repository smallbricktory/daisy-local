# diagnostics/

One-off analysis + measurement tools — kept separate from shippable code, but
wired into a crate as cargo examples (via `[[example]] path = ...`) so they stay
buildable as the code evolves. Not part of any release artifact.

Grouped by what they measure. The owning crate is whichever one supplies the
code under test, which is why `-p <crate>` differs per tool:

```sh
cargo run --release -p <crate> --example <name> -- <args>
```

## audio/ — capture, echo cancellation, denoise

| example | crate | what it answers |
|---|---|---|
| `wasapi_smoke` | audio-engine | Does WASAPI capture open and deliver frames on this Windows box? |
| `wasapi_dual_probe` | audio-engine | Do mic + loopback capture concurrently, and at what relative clock? |
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

## speakers/ — diarization and voiceprints

| example | crate | what it answers |
|---|---|---|
| `diag_diarize` | voiceprints | The big one: embeddings, cluster assignments, silhouette, and pairwise-cosine distributions for a session. |
| `denoise_diar_ab` | voiceprints | Does DFN3 denoise change diarization? Embeds the same mic-track segments from `mic_dn` (denoised) vs `mic_aec` (echo-cancelled fallback) and reports per-segment drift, cluster count, silhouette, assignment agreement. It didn't move the outcome across 6 sessions — which is why denoise is opt-in and playback-only. |
| `probe_vp` | voiceprints | Embeds a WAV and prints its voiceprint vector. |
| `probe_diarize_mic` | voiceprints | Diarizes the mic track alone — the many-people-on-one-in-person-mic case. |
| `time_speakrs` | voiceprints | Times the speakrs (pyannote) backend; needs `SPEAKRS_MODELS_DIR`. |

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

Drop the file in the matching subdirectory and add the target to the owning
crate's `Cargo.toml`:

```toml
[[example]]
name = "my_probe"
path = "../../diagnostics/audio/my_probe.rs"
```

`cargo build --workspace --examples` then covers it. `cargo clippy --workspace
--all-targets` lints these too, and a deny-by-default lint in one of them
aborts the run for the entire workspace — so keep them clean.
