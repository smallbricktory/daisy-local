#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Run the parakeet ASR suites on Windows, from WSL.
#
#   scripts/test-windows-asr.sh [model_dir] [wav]
#
# Linux runs only type-check the ort decode path against the Windows target;
# this stages the tree to the Windows side and runs the providers-local
# parakeet tests natively, so the int8 ONNX kernels actually execute. With a
# model dir (and optionally a 16 kHz wav) the gated contract + livesim suites
# run too and print decode timings — the Windows int8 encoder is a known ~4×
# slower than Linux at short windows; timings far worse than that are a stop
# signal.
#
# Prereq: Windows interop on this host, with a Rust toolchain on the Windows
# side (the same one scripts/build-release.ps1 uses).
set -uo pipefail

source "$(cd "$(dirname "$0")" && pwd)/lib/stage-windows.sh"

MODEL_DIR="${1:-}"
WAV="${2:-}"

stage_windows || exit 1

ENV_PREFIX=""
if [ -n "$MODEL_DIR" ]; then
  WIN_MODELS='C:\tmp\daisy-asr-model'
  echo "staging parakeet model dir → $WIN_MODELS"
  rsync -a --delete "$MODEL_DIR"/ /mnt/c/tmp/daisy-asr-model/ || exit 1
  ENV_PREFIX="\$env:DAISY_PARAKEET_TEST_MODEL_DIR='$WIN_MODELS'; "
fi
if [ -n "$WAV" ]; then
  cp -f "$WAV" /mnt/c/tmp/daisy-asr-model.wav || exit 1
  ENV_PREFIX="$ENV_PREFIX\$env:DAISY_PARAKEET_WAV='C:\\tmp\\daisy-asr-model.wav'; "
fi

OUT=$(mktemp /tmp/daisy-winasr-XXXX.log)
# whisper-rs-sys builds in this crate too: same env as build-release.ps1 —
# Vulkan SDK on PATH, cmake policy floor, short target dir (MAX_PATH).
WIN_ENV="if (-not \$env:CMAKE_POLICY_VERSION_MINIMUM) { \$env:CMAKE_POLICY_VERSION_MINIMUM = '3.5' }; \
if (-not \$env:VULKAN_SDK) { \$vk = Get-ChildItem 'C:\\VulkanSDK' -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1; if (\$vk) { \$env:VULKAN_SDK = \$vk.FullName } }; \
if (\$env:VULKAN_SDK) { \$env:PATH = \"\$(\$env:VULKAN_SDK)\\Bin;\$(\$env:PATH)\" }; \
if (-not \$env:CARGO_TARGET_DIR) { \$env:CARGO_TARGET_DIR = 'D:\\b' }; "
# The whole providers-local suite: units plus the env-gated contract +
# livesim suites (those self-skip without the model env — no name filter, a
# filter would silently exclude them).
$PS -NoProfile -Command \
  "cd C:\\tmp\\daisy-build; ${WIN_ENV}${ENV_PREFIX}cargo test -p providers-local -- --nocapture; 'WIN_TEST_RC='+\$LASTEXITCODE" \
  >"$OUT"
sed 's/\r$//' "$OUT" | grep -E "^(test |running |test result|streamed|batch|speech words|skip|error|WIN_TEST_RC)" || true

if grep -q 'WIN_TEST_RC=0' "$OUT"; then
  echo "windows parakeet tests: PASSED"
  rm -f "$OUT"
  exit 0
fi
echo "windows parakeet tests: FAILED (full log: $OUT)"
exit 1
