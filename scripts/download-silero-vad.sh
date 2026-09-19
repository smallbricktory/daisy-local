#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fetch the Silero VAD v5 ONNX model (~2.3MB, MIT) for the faint-audio marker
# detector (crates/providers-local/src/streaming/vad.rs). Ships in the
# win/linux bundle at models/vad/. SHA-pinned so a future upstream change
# (which could break the v5 input/state/sr contract) fails loudly instead of
# silently shipping a mismatched model. Idempotent.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/vad"
mkdir -p "$out"

url="https://github.com/snakers4/silero-vad/raw/master/src/silero_vad/data/silero_vad.onnx"
sha256="1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
dest="$out/silero_vad_v5.onnx"

if [ -f "$dest" ] && echo "$sha256  $dest" | sha256sum -c - >/dev/null 2>&1; then
  echo "silero_vad_v5.onnx present + verified"; exit 0
fi
curl -sSL -o "$dest" "$url"
echo "$sha256  $dest" | sha256sum -c - \
  || { echo "silero_vad_v5.onnx SHA mismatch — upstream changed; re-verify the v5 contract"; exit 1; }
echo "downloaded + verified $dest ($(du -h "$dest" | cut -f1))"
