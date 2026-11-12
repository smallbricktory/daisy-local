#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fetches the WeSpeaker speaker-embedding ONNX model used by the
# voiceprint pipeline (~26 MB, CC-BY-4.0) and the pyannote segmentation-3.0
# ONNX the diarizer runs (~6 MB, MIT). Attribution for both is in
# scripts/lib/model-notices.txt.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/voiceprints"
mkdir -p "$out"

# The Wespeaker authors' ResNet34-LM model, trained on VoxCeleb. Output:
# 256-d speaker embedding.
base="https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM/resolve/main"

fetch() {
    local rel="$1"
    local name="$2"
    local dst="$out/$name"
    if [ -s "$dst" ]; then
        echo "exists: $name ($(stat -c%s "$dst" 2>/dev/null || stat -f%z "$dst") bytes)"
        return 0
    fi
    echo "fetching $rel -> $dst"
    curl --fail --location --output "$dst.tmp" "$base/$rel"
    mv "$dst.tmp" "$dst"
}

fetch "voxceleb_resnet34_LM.onnx" "model.onnx"

echo "done. voiceprint model at $out"
ls -lh "$out"

# ── pyannote segmentation-3.0 (MIT) ───────────────────────────────────────────
# ONNX export of the speaker-segmentation network the diarizer runs
# (voiceprints::segmentation). Lives beside the voiceprint model.
seg="$out/segmentation-3.0.onnx"
if [ -s "$seg" ]; then
    echo "exists: segmentation-3.0.onnx ($(stat -c%s "$seg" 2>/dev/null || stat -f%z "$seg") bytes)"
else
    echo "fetching segmentation-3.0.onnx"
    curl --fail --location --output "$seg.tmp" \
        "https://huggingface.co/avencera/speakrs-models/resolve/main/segmentation-3.0.onnx"
    mv "$seg.tmp" "$seg"
fi
echo "done. segmentation model at $seg"
ls -lh "$out"
