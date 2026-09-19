#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Run the Windows audio backend's tests on Windows, from WSL.
#
#   scripts/test-windows-backend.sh
#
# `cargo test` here never compiles crates/audio-engine/src/windows — the module
# is cfg-gated to Windows — and `cargo check --target x86_64-pc-windows-msvc`
# only type-checks it. This stages the tree to the Windows side and runs the
# tests natively through the interop shell, so WASAPI format parsing and buffer
# decoding are actually executed.
#
# Prereq: Windows interop on this host, with a Rust toolchain on the Windows
# side (the same one scripts/build-release.ps1 uses).
set -uo pipefail

source "$(cd "$(dirname "$0")" && pwd)/lib/stage-windows.sh"

stage_windows || exit 1

# No inner 2>&1: cargo writes progress to stderr and $ErrorActionPreference
# would abort on it.
OUT=$(mktemp /tmp/daisy-wintest-XXXX.log)
# --lib on purpose, unlike the tauri-app rule: the Windows-only code lives in
# the library, while crates/audio-engine/tests is platform-independent and
# already runs on Linux in the normal gate.
$PS -NoProfile -Command \
  "cd C:\\tmp\\daisy-build; cargo test -p audio-engine --lib; 'WIN_TEST_RC='+\$LASTEXITCODE" \
  >"$OUT"
sed 's/\r$//' "$OUT" | grep -E "^(test |running |test result|error|warning: unused|WIN_TEST_RC)" || true

if grep -q 'WIN_TEST_RC=0' "$OUT"; then
  echo "windows backend tests: PASSED"
  rm -f "$OUT"
  exit 0
fi
echo "windows backend tests: FAILED (full log: $OUT)"
exit 1
