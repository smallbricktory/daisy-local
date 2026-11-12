#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Builds the daisy-llm worker in release and stages it where tauri-build
# expects a sidecar: crates/tauri-app/binaries/daisy-llm-<host triple>.
# Every tauri-app build (dev or release) copies it next to daisy-app.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
triple="$(rustc -vV | sed -n 's/^host: //p')"
[ -n "$triple" ] || { echo "rustc -vV printed no host triple" >&2; exit 1; }
cargo build -p daisy-llm --release --locked
out="crates/tauri-app/binaries"
mkdir -p "$out"
src="${CARGO_TARGET_DIR:-target}/release/daisy-llm"
cp -f "$src" "$out/daisy-llm-$triple"
chmod +x "$out/daisy-llm-$triple"
echo "staged $out/daisy-llm-$triple ($(du -h "$out/daisy-llm-$triple" | cut -f1))"
