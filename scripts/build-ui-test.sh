#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the e2e binary: shipping code + the wdio plugins. Output:
# $CARGO_TARGET_DIR/release/daisy-app (raw binary, no bundle), target/release
# when the var is unset. The frontend goes to apps/frontend/dist-ui-test; the
# shipping apps/frontend/dist is not written. The release profile runs with LTO
# off and 16 codegen units; opt-level stays 3.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
# Stages the bundle resources; the download scripts keep files whose sha256 already matches.
for m in voiceprints nemotron nemotron3-diar embeddings; do bash "scripts/download-$m.sh"; done
( cd apps/frontend && VITE_UI_TEST=1 npx pnpm@10 build )
( cd crates/tauri-app && CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    cargo tauri build --no-bundle --features ui-test --config tauri.ui-test.conf.json )
ls -la "${CARGO_TARGET_DIR:-target}/release/daisy-app"
