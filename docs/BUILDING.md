# Building Daisy

Daisy is a Rust workspace + React frontend. Linux, Windows, and macOS all
ship from the same source: Linux as AppImage + .deb, Windows as an NSIS
installer + portable zip, macOS as a dmg.

## Models the build expects (all platforms, before first build)

`tauri.conf.json` declares the bundled model files under `bundle.resources`,
and the Tauri build script resolves those paths during **any**
`cargo build -p tauri-app` — a fresh checkout fails with
`resource path ... doesn't exist` until they are staged:

```bash
bash scripts/download-whisper.sh       # bundled Whisper base.en model
bash scripts/download-embeddings.sh    # Q&A/RAG embedding model (BGE-small)
bash scripts/download-voiceprints.sh   # voiceprint model + speakrs diarization set (ONNX + PLDA)
```

The last three pull public Hugging Face assets. The DTLN-aec model comes
from upstream ([breizhn/DTLN-aec](https://github.com/breizhn/DTLN-aec),
MIT) as TFLite weights. The DTLN-aec pair is committed in-tree at
`models/dtln-aec/`; `bash models/dtln-aec/convert-from-upstream.sh`
reproduces it from the upstream weights (details in
`models/dtln-aec/README.md`). On Windows the scripts run under Git Bash
or WSL (that is how `scripts/build-release.ps1` invokes them).

The scripts are idempotent — re-running skips files already present.

## Linux (primary)

System deps + Rust + Node toolchain, then models (above):

```bash
bash scripts/setup-dev.sh   # apt deps, rustup, node, pnpm
```

Build + run. The `custom-protocol` feature snapshots `apps/frontend/dist/`
into the binary at compile time, so the frontend must be built first:

```bash
pnpm --dir apps/frontend install --frozen-lockfile
pnpm --dir apps/frontend build
cargo build --release -p tauri-app --features custom-protocol
./target/release/daisy-app
```

The `daisy` CLI (headless transcribe/summarize against an existing profile)
is a separate binary: `cargo build --release -p daisy-cli`.

Release artifacts (AppImage + .deb in `dist/`):

```bash
bash scripts/build-release.sh
```

Runs inside a containerized Ubuntu 24.04 image (the floor we target) so the
resulting AppImage is portable forward to 25.04 / 26.04.

Whisper's Linux backends — Vulkan GPU offload (CPU fallback at runtime) and
OpenBLAS — are unconditional target dependencies, so **every** build (dev and
release) compiles them and needs `libopenblas-dev`, `libvulkan-dev`, and
`glslc` installed (`scripts/setup-dev.sh` handles it). BLAS header paths come
from `.cargo/config.toml` (`BLAS_INCLUDE_DIRS`).

## Windows

Native build with the MSVC toolchain.

### Prerequisites

One-time setup beyond the usual Rust + Node story:

| Tool | Why | Install |
|---|---|---|
| Visual Studio Build Tools 2022, Desktop C++ workload | MSVC linker `link.exe` | `winget install Microsoft.VisualStudio.2022.BuildTools` then GUI workload select, or pass `--override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` |
| Rust stable, x86_64-pc-windows-msvc default | The toolchain | `rustup-init.exe -y --default-host x86_64-pc-windows-msvc` |
| Node 20+ and pnpm 10+ | Frontend build | `winget install OpenJS.NodeJS.LTS` then `npm install -g pnpm@10` |
| CMake 3.5+ (4.x is fine) | `audiopus_sys`, `whisper-rs-sys` build their C deps via cmake-rs | `winget install Kitware.CMake` |
| LLVM/Clang (libclang) | `whisper-rs-sys` runs bindgen against whisper.cpp headers | `winget install LLVM.LLVM` |
| Vulkan SDK (`glslc`) | whisper's Vulkan backend is an unconditional Windows target dep — every build compiles its shaders | [vulkan.lunarg.com](https://vulkan.lunarg.com/sdk/home) — ensure `VULKAN_SDK` is set and `%VULKAN_SDK%\Bin` on `PATH` |

Environment variables every Windows build session needs:

```powershell
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
# CMake 4 deprecated cmake_minimum_required < 3.5; bundled C deps
# (whisper.cpp, libopus) still declare 3.0–3.4 baselines.
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
# whisper.cpp's vulkan-shaders-gen overflows Windows MAX_PATH (260) under a
# deep target/ path — build into a short directory.
$env:CARGO_TARGET_DIR = "D:\b"
```

If you already have an older `winget install LLVM.LLVM` (clang ≥ 22.x),
nothing extra needed — the workspace pins `whisper-rs = "0.16"` which
pulls a bindgen new enough to survive clang 22's API shift (see
[whisper-rs#268](https://codeberg.org/tazz4843/whisper-rs/issues/268)).

### Build

PowerShell, from the repo root, after the prerequisites and the model
downloads above:

```powershell
# Frontend (Tauri's beforeBuildCommand mis-resolves cwd, so we build it
# manually here — same pattern as scripts/build-inside.sh on Linux):
pnpm --dir apps/frontend install --frozen-lockfile
pnpm --dir apps/frontend build

# App binary (bundles the frontend dist)
cargo build --release -p tauri-app --features custom-protocol --locked
.\target\release\daisy-app.exe
```

For the NSIS installer:

```powershell
cargo install --locked tauri-cli   # one-time
cd crates\tauri-app
cargo tauri build --bundles nsis
# Lands in target\release\bundle\nsis\
```

This is the shipped installer format; `scripts/build-release.ps1` is the
full release pipeline (frontend + models + NSIS + portable zip + optional
Azure code signing). The first Tauri-bundle run downloads NSIS (~3 MB)
automatically. `beforeBuildCommand` is left empty in `tauri.conf.json`;
the frontend was already built above.

### Frontend-rebuild gotcha

The `custom-protocol` feature snapshots `apps/frontend/dist/` into the
binary at compile time via `tauri::generate_context!()`. A
cargo-only rebuild keeps the OLD dist baked in — so any
frontend-touching commit means **both** of these, in order:

```powershell
pnpm --dir apps/frontend build
cargo build --release -p tauri-app --features custom-protocol
```

Pure-Rust changes can skip the `pnpm` step. Symptoms when this is
missed: app launches, but a UI change you can see in `git log` isn't
visible in the running window.

First app launch needs the Microsoft WebView2 runtime; modern Windows
ships it preinstalled. If `daisy-app.exe` errors with a WebView2-missing
dialog, install from <https://developer.microsoft.com/microsoft-edge/webview2/>.

## CI

`.github/workflows/ci.yml` runs three jobs on every push to `main` and
every PR:

- **version guard** — `Cargo.toml` and `tauri.conf.json` versions must
  match; notices (non-blocking) when the version is untagged.
- **linux (full workspace)** — builds the frontend + whole workspace and
  runs every test except `audio-engine` (PipeWire daemon required) and
  `recording` (one test needs a real audio device).
- **windows (portable crates + audio-engine seam)** — cross-builds
  `audio-engine` to verify the `#[cfg(target_os = "windows")]` seam
  stays clean, plus builds and tests every cross-platform crate.

## macOS

Apple Silicon. Requires Xcode command-line tools, Homebrew `cmake`,
`node`, `pnpm`, and `cargo-tauri`. Transcription uses Metal; diarization
uses CoreML.

```bash
bash scripts/build-release-mac.sh
```

Produces `Daisy.app` under `target/release/bundle/macos/`. Signing and
notarization apply only when the corresponding credentials are
configured; unsigned builds run locally via right-click → Open.
