# diagnostics/

One-off analysis + measurement tools — kept separate from shippable code, but
wired into a crate as cargo examples (via `[[example]] path = ...`) so they stay
buildable as the code evolves. Not part of any release artifact.

## Tools

- **denoise_diar_ab.rs** — does DFN3 denoise actually change diarization? For a
  session, embeds the same mic-track segments from `mic_dn` (denoised) vs
  `mic_aec` (echo-cancelled fallback) and reports per-segment embedding drift,
  cluster count, silhouette, and assignment agreement. Used to decide denoise is
  opt-in/playback-only (it didn't change diarization outcomes across 6 sessions).

  ```sh
  DAISY_VOICEPRINT_DIR=$PWD/models/voiceprints \
    cargo run --release -p voiceprints --example denoise_diar_ab -- <session_dir> [known_k]
  ```

Older diagnostics still live under each crate's `examples/` (e.g.
`crates/voiceprints/examples/diag_diarize.rs`); new ones go here.
