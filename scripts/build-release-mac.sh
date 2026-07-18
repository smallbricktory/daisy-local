#!/usr/bin/env bash
# macOS release build for Daisy. Runs on the mac-host self-hosted runner (or any
# Apple-Silicon mac with Rust + Node + pnpm + cargo-tauri + cmake). Produces a
# .dmg copied to dist/Daisy_<version>_aarch64.dmg.
#
# Mirrors build-release.sh / build-release.ps1: stage models, build the daisy-cli
# sidecar, build the frontend, then `cargo tauri build` the app + dmg with the
# Metal whisper backend.
#
# SIGNING / NOTARIZATION: `cargo tauri build` signs + notarizes + staples during
# bundling when these env vars are present (CI passes them from secrets):
#   APPLE_SIGNING_IDENTITY  "Developer ID Application: … (TEAMID)"
#   APPLE_API_KEY           App Store Connect API Key ID
#   APPLE_API_ISSUER        App Store Connect Issuer ID
#   APPLE_API_KEY_PATH      path to AuthKey_<id>.p8 (kept on-box, not in secrets)
# Absent the identity, the build is ad-hoc-signed (local dev) and the CI verify
# gate blocks publish. Entitlements: crates/tauri-app/Daisy.entitlements.
set -euo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
# Newer CMake rejects vendored CMakeLists with policy min <3.5 (audiopus/onnx).
export CMAKE_POLICY_VERSION_MINIMUM="${CMAKE_POLICY_VERSION_MINIMUM:-3.5}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"
export CMAKE_OSX_DEPLOYMENT_TARGET="${CMAKE_OSX_DEPLOYMENT_TARGET:-11.0}"

VERSION="$(grep -m1 '^version' crates/tauri-app/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
echo "=== mac build: daisy $VERSION | $(rustc --version) | macOS $(sw_vers -productVersion) | sha=${DAISY_BUILD_SHA:-unknown} ==="

# Stage models (same sources as the Linux/Windows CI; idempotent).
# Model files (ggml/onnx) are platform-agnostic. All plain-HTTPS fetches.
bash scripts/download-whisper.sh
bash scripts/download-voiceprints.sh
bash scripts/download-embeddings.sh

# speakrs native-CoreML models (.mlmodelc, ~407M, macOS-only).
# download-voiceprints fetches the ONNX + PLDA set; the .mlmodelc set is not
# covered by that fetch and is overlaid from the speakrs HF cache before
# bundling. Without it, from_dir(CoreMl) fails on the shipped app and speakrs
# silently falls back to k-means. Fails loud when the cache is absent.
SPEAKRS_HF="$(ls -d "$HOME"/.cache/huggingface/hub/models--avencera--speakrs-models/snapshots/*/ 2>/dev/null | head -1)"
if [ -n "$SPEAKRS_HF" ] && ls "$SPEAKRS_HF"*.mlmodelc >/dev/null 2>&1; then
  cp -RL "$SPEAKRS_HF"*.mlmodelc models/speakrs/
  echo "speakrs: injected $(ls -d models/speakrs/*.mlmodelc | wc -l | tr -d ' ') mlmodelc dirs ($(du -sh models/speakrs | cut -f1)) into bundle"
else
  echo "FATAL: speakrs .mlmodelc not in HF cache ($SPEAKRS_HF) — CoreML speakrs would ship dead (k-means fallback)" >&2
  exit 1
fi

# Frontend (compiled into the binary).
( cd apps/frontend && pnpm install --frozen-lockfile && pnpm build )
# Forces a relink; the fresh dist is re-embedded. rerun-if-changed on dist
# is unreliable across clean checkouts.
touch crates/tauri-app/src/main.rs

# Locates the App Store Connect API key on the build machine (kept out of
# the repo + CI secrets); tauri uses it to notarize. Only required when
# signing.
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  if [ -z "${APPLE_API_KEY_PATH:-}" ]; then
    APPLE_API_KEY_PATH="$(ls "$HOME"/.ssh/AuthKey_*.p8 2>/dev/null | head -1)"
  fi
  [ -n "${APPLE_API_KEY_PATH:-}" ] || { echo "FATAL: APPLE_SIGNING_IDENTITY set but no AuthKey_*.p8 found for notarization" >&2; exit 1; }
  export APPLE_API_KEY_PATH
  echo "=== signing: $APPLE_SIGNING_IDENTITY | notarize key $(basename "$APPLE_API_KEY_PATH") ==="
else
  echo "=== APPLE_SIGNING_IDENTITY unset — ad-hoc build (local dev; CI verify gate blocks publish) ==="
fi

# Bundle app + dmg. The Metal whisper backend is a macOS target dep of
# providers-local. Signs + notarizes + staples when the APPLE_* env is set
# (see above).
cargo tauri build --bundles app,dmg

# Collect the dmg into dist/ under a stable name (skip tauri's rw.* intermediate).
mkdir -p dist
DMG="$(find target -path '*/release/bundle/dmg/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
if [ -z "$DMG" ]; then
  DMG="$(find target -path '*/release/bundle/macos/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
fi
[ -n "$DMG" ] || { echo "no .dmg produced under target/**/release/bundle" >&2; exit 1; }
cp -f "$DMG" "dist/Daisy_${VERSION}_aarch64.dmg"
echo "=== built dist/Daisy_${VERSION}_aarch64.dmg ==="
