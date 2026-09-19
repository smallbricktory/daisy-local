#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the e2e binary: shipping code + the wdio plugins. Output:
# $CARGO_TARGET_DIR/release/daisy-app (raw binary, no bundle), target/release
# when the var is unset. The frontend goes to apps/frontend/dist-ui-test; the
# shipping apps/frontend/dist is not written.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
( cd apps/frontend && VITE_UI_TEST=1 npx pnpm@10 build )
( cd crates/tauri-app && cargo tauri build --no-bundle --features ui-test \
    --config tauri.ui-test.conf.json )
ls -la "${CARGO_TARGET_DIR:-target}/release/daisy-app"
