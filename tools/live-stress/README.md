# Live-ASR catch-up controller — stress tools

Interactive load generators for testing the live-whisper catch-up controller
(`crates/providers-local/src/streaming/controller.rs`). Drive contention while a
Daisy recording runs, then watch the controller react in the perf log:

```
whisper=[decode:Nms wait:Nms win:Nms backlog:Nms(max N) n:N hop:Nms rho:N.NN FLOOR]
```

As `rho` climbs the controller raises `hop` (1000→1500→2000→…); `FLOOR` appears
when even the max hop can't keep up (extreme load). The log lives in the profile:
`<profile>/logs/daisy-<host>-<date>.log` (Windows: `%APPDATA%\daisy\Daisy\data\logs\`).
Tail it: `Get-Content <log> -Wait | Select-String 'hop:'`.

## `stress.ps1` — concurrent transcriptions (faithful)
Runs N looping whisper/Vulkan decodes that fight the live decoder for the same
GPU. The realistic contention (mirrors a video call hammering the GPU).

```
powershell -ExecutionPolicy Bypass -File stress.ps1
# prompt: type 1/2/3 (or +/-) to ramp concurrent transcriptions, 0=off, q=quit
```

Needs (defaults, override with `-Bench/-Model/-Wav`):
- `whisper_bench.exe` built with the `vulkan` feature
  (`cargo build --release -p providers-local --bin whisper_bench --features vulkan`,
  lands at `<CARGO_TARGET_DIR>\release\whisper_bench.exe`).
- a ggml whisper model (e.g. `ggml-base.en.bin`).
- a 16 kHz mono WAV of sustained speech, 10+ minutes.

## `bloat.ps1` — CPU + RAM hog (secondary)
Type a level = that many busy cores + ~300 MB each. Moves `rho` less than
`stress.ps1` (doesn't touch the GPU) but useful for CPU/memory pressure.

## Notes
- Windows/PowerShell only (the GPU contention path is Vulkan via whisper_bench).
- Stress workers spawn their own whisper contexts (separate processes) — fine,
  they just compete for the GPU like a real call.
- For combined load, also run a real Teams/Zoom call or a `cargo build`.
