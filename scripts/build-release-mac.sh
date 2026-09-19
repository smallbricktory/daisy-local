#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# macOS release build for Daisy. Runs on the mac-host self-hosted runner (or any
# Apple-Silicon mac with Rust + Node + pnpm + cargo-tauri + cmake). Produces a
# .dmg copied to dist/Daisy_<version>_aarch64.dmg.
#
# Mirrors build-release.sh / build-release.ps1: stage models, build the frontend,
# then `cargo tauri build` the app + dmg with the Metal whisper backend. daisy-cli
# is a dev tool and does not ship.
#
# SIGNING / NOTARIZATION: `cargo tauri build` signs + notarizes + staples the
# .app during bundling when these env vars are present (CI passes them from
# secrets); this script then notarizes + staples the dmg wrapper itself, which
# tauri does not cover:
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

# Machine-local signing config (never in the repo): may export
# APPLE_SIGNING_IDENTITY / APPLE_API_KEY / APPLE_API_ISSUER / APPLE_API_KEY_PATH.
# Values already in the environment win over the file.
if [ -f "$HOME/.config/daisy/signing.env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.config/daisy/signing.env"
fi

# Build id: env override > stamp file staged by build-local-all.sh > git >
# interactive prompt. An unstamped release binary fails verification, so
# refuse to guess.
if [ -z "${DAISY_BUILD_SHA:-}" ]; then
  if [ -f .daisy-build-sha ]; then
    DAISY_BUILD_SHA="$(cat .daisy-build-sha)"
  elif git rev-parse --short HEAD >/dev/null 2>&1; then
    DAISY_BUILD_SHA="$(git rev-parse --short HEAD)"
  elif [ -t 0 ]; then
    read -rp "DAISY_BUILD_SHA (git short sha of the commit being built): " DAISY_BUILD_SHA
  fi
  [ -n "${DAISY_BUILD_SHA:-}" ] || { echo "FATAL: DAISY_BUILD_SHA unset and no .daisy-build-sha/git available" >&2; exit 1; }
  export DAISY_BUILD_SHA
fi

echo "=== mac build: daisy $VERSION | $(rustc --version) | macOS $(sw_vers -productVersion) | sha=$DAISY_BUILD_SHA ==="

# Stage models (same sources as the Linux/Windows CI; idempotent).
# Model files are platform-agnostic. All plain-HTTPS fetches. The mac bundle
# ships no ASR model: whisper models are user downloads, parakeet is the
# win/linux floor, and SpeechAnalyzer is OS-managed.
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

# Frontend (compiled into the binary). VITE_UI_TEST=1 sends the bundle to
# dist-ui-test; the shipping dist comes from this run.
unset VITE_UI_TEST
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
  # The key id is the filename: AuthKey_<id>.p8.
  if [ -z "${APPLE_API_KEY:-}" ]; then
    APPLE_API_KEY="$(basename "$APPLE_API_KEY_PATH" | sed -E 's/^AuthKey_(.+)\.p8$/\1/')"
    export APPLE_API_KEY
  fi
  [ -n "${APPLE_API_ISSUER:-}" ] || { echo "FATAL: APPLE_API_ISSUER unset (put it in ~/.config/daisy/signing.env)" >&2; exit 1; }
  echo "=== signing: $APPLE_SIGNING_IDENTITY | notarize key $(basename "$APPLE_API_KEY_PATH") ==="
else
  echo "=== APPLE_SIGNING_IDENTITY unset — ad-hoc build (local dev; CI verify gate blocks publish) ==="
fi

# Bundle app + dmg. The Metal whisper backend is a macOS target dep of
# providers-local. Signs + notarizes + staples when the APPLE_* env is set
# (see above).
echo "=== staging daisy-llm sidecar ==="
bash "$(dirname "$0")/stage-daisy-llm.sh"
cargo tauri build --bundles app,dmg

# Collect the dmg into dist/ under a stable name (skip tauri's rw.* intermediate).
mkdir -p dist
DMG="$(find target -path '*/release/bundle/dmg/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
if [ -z "$DMG" ]; then
  DMG="$(find target -path '*/release/bundle/macos/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
fi
[ -n "$DMG" ] || { echo "no .dmg produced under target/**/release/bundle" >&2; exit 1; }
# The mac bundle ships no ASR model (no parakeet dir, no whisper ggml).
APP="$(find target -path '*/release/bundle/macos/*.app' -maxdepth 6 2>/dev/null | head -1)"
if [ -n "$APP" ]; then
  if [ -n "$(find "$APP" \( -path '*models/parakeet*' -o -name 'ggml-*.bin' \) -print -quit)" ]; then
    echo "FATAL: an ASR model is inside the mac bundle — mac ships none" >&2
    exit 1
  fi
  SIDE="$APP/Contents/MacOS/daisy-llm"
  [ -f "$SIDE" ] || { echo "FATAL: daisy-llm missing from $APP/Contents/MacOS" >&2; exit 1; }
  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
    # Read once and match in the shell: `| grep -q` ends the pipe on its
    # first match, and under `pipefail` codesign's SIGPIPE fails the check
    # that just passed.
    SIDE_SIG="$(codesign -dv --verbose=2 "$SIDE" 2>&1 || true)"
    case "$SIDE_SIG" in
      *"Runtime Version"*) ;;
      *) echo "FATAL: daisy-llm is not signed with the hardened runtime" >&2; exit 1 ;;
    esac
  fi
fi
OUT="dist/Daisy_${VERSION}_aarch64.dmg"
cp -f "$DMG" "$OUT"

# The .app inside carries its own ticket, but a dmg without one fails
# Gatekeeper on offline first-open. Staple fails loud when the submission
# was rejected, so a bad dmg never leaves this script quietly.
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  echo "=== notarizing dmg wrapper ==="
  xcrun notarytool submit "$OUT" \
    --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" \
    --issuer "$APPLE_API_ISSUER" --wait
  xcrun stapler staple "$OUT"
  xcrun stapler validate "$OUT"
fi
echo "=== built $OUT ==="
