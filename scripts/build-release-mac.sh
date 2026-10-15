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
#   APPLE_API_KEY_PATH      path to AuthKey_<id>.p8
# Absent the identity, the build is ad-hoc-signed (local dev) and the CI verify
# gate blocks publish. Entitlements: crates/tauri-app/Daisy.entitlements.
set -euo pipefail

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
# Sets the CMake policy floor for vendored CMakeLists.
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

# Build id: env override > stamp file > git > interactive prompt.
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
# The mac bundle ships no ASR model.
bash scripts/download-voiceprints.sh
bash scripts/download-embeddings.sh
# Live speaker-name diarizer: bundled in every desktop artifact.
bash scripts/download-nemotron3-diar.sh

# Frontend (compiled into the binary).
unset VITE_UI_TEST
( cd apps/frontend && pnpm install --frozen-lockfile && pnpm build )
# Forces a relink so the fresh dist is embedded.
touch crates/tauri-app/src/main.rs

# Locates the App Store Connect API key tauri uses to notarize. Required only
# when signing.
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
# The rw.* image a failed Finder step leaves behind, still attached under
# /Volumes/dmg.*: detached (forced, other processes may hold it) and deleted.
drop_rw_images() {
  hdiutil info | awk -v t="$PWD/target/" '/^image-path/{p=$3}
    /^\/dev\/disk[0-9]+[ \t]/{ if (index(p,t)==1 && p ~ /\/rw\.[0-9]+\.[^\/]*\.dmg$/) print $1 }' |
    while read -r dev; do hdiutil detach -force "$dev" >/dev/null 2>&1 || true; done
  find target -path '*/release/bundle/*' -name 'rw.*.dmg' -delete 2>/dev/null || true
}
# Earlier runs' dmgs go first, so the collect step below finds only this one.
drop_rw_images
find target -path '*/release/bundle/*' -name '*.dmg' -delete 2>/dev/null || true
rm -f dist/Daisy_*_aarch64.dmg
trap drop_rw_images EXIT
cargo tauri build --bundles app,dmg

# Collect the dmg into dist/ under a stable name (skip tauri's rw.* intermediate).
mkdir -p dist
DMG="$(find target -path '*/release/bundle/dmg/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
if [ -z "$DMG" ]; then
  DMG="$(find target -path '*/release/bundle/macos/*.dmg' 2>/dev/null | grep -v '/rw\.' | head -1)"
fi
[ -n "$DMG" ] || { echo "no .dmg produced under target/**/release/bundle" >&2; exit 1; }
# The mac bundle ships no ASR model (no nemotron dir, no whisper ggml); the
# diarizer under models/nemotron3-diar is not one.
APP="$(find target -path '*/release/bundle/macos/*.app' -maxdepth 6 2>/dev/null | head -1)"
[ -n "$APP" ] || { echo "no .app produced under target/**/release/bundle" >&2; exit 1; }
bash "$(dirname "$0")/check-no-license-test.sh" "$APP/Contents/MacOS/daisy-app"
bash "$(dirname "$0")/check-mac-linkage.sh" "$APP"
if [ -n "$APP" ]; then
  if [ -n "$(find "$APP" \( -path '*models/nemotron/*' -o -name 'ggml-*.bin' \) -print -quit)" ]; then
    echo "FATAL: an ASR model is inside the mac bundle — mac ships none" >&2
    exit 1
  fi
  [ -f "$APP/Contents/Resources/models/nemotron3-diar/nemotron3_diar_fast32.onnx" ] \
    || { echo "FATAL: nemotron3-diar model missing from $APP/Contents/Resources/models" >&2; exit 1; }
  SIDE="$APP/Contents/MacOS/daisy-llm"
  [ -f "$SIDE" ] || { echo "FATAL: daisy-llm missing from $APP/Contents/MacOS" >&2; exit 1; }
  if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
    SIDE_SIG="$(codesign -dv --verbose=2 "$SIDE" 2>&1 || true)"
    case "$SIDE_SIG" in
      *"Runtime Version"*) ;;
      *) echo "FATAL: daisy-llm is not signed with the hardened runtime" >&2; exit 1 ;;
    esac
  fi
fi
OUT="dist/Daisy_${VERSION}_aarch64.dmg"
cp -f "$DMG" "$OUT"

# Notarizes and staples the dmg wrapper; staple fails when the submission was rejected.
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ]; then
  echo "=== notarizing dmg wrapper ==="
  xcrun notarytool submit "$OUT" \
    --key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY" \
    --issuer "$APPLE_API_ISSUER" --wait
  xcrun stapler staple "$OUT"
  xcrun stapler validate "$OUT"
fi
echo "=== built $OUT ==="
