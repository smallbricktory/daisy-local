#!/usr/bin/env bash
# Fetches the WeSpeaker speaker-embedding ONNX export used by the
# voiceprint pipeline. ~26 MB on disk. License is Apache-2.0 / MIT-compatible
# (model card on HF); safe to redistribute inside the AppImage.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/voiceprints"
mkdir -p "$out"

# `hbredin/wespeaker-voxceleb-resnet34-LM` is the pyannote-author-published
# ONNX export of the standard WeSpeaker ResNet34 model. Output: 256-d
# speaker embedding, trained on VoxCeleb1+2.
base="https://huggingface.co/hbredin/wespeaker-voxceleb-resnet34-LM/resolve/main"

fetch() {
    local rel="$1"
    local name="$2"
    local dst="$out/$name"
    if [ -s "$dst" ]; then
        echo "exists: $name ($(stat -c%s "$dst") bytes)"
        return 0
    fi
    echo "fetching $rel -> $dst"
    curl --fail --location --output "$dst.tmp" "$base/$rel"
    mv "$dst.tmp" "$dst"
}

fetch "speaker-embedding.onnx" "model.onnx"

echo "done. voiceprint model at $out"
ls -lh "$out"

# ── speakrs diarization model set (pyannote community-1, CC-BY-4.0) ───────────
# The ONNX (CPU) + PLDA set speakrs `from_dir` loads: segmentation-3.0 +
# wespeaker-voxceleb-resnet34 (+ external weights + min-samples) + the 6 VBx
# PLDA matrices. Bundled to models/speakrs/ and surfaced at startup as
# SPEAKRS_MODELS_DIR. The native-CoreML .mlmodelc set (macOS) is layered in
# separately by build-release-mac.sh. ~57 MB.
sp_out="$repo_root/models/speakrs"
sp_base="https://huggingface.co/avencera/speakrs-models/resolve/main"
mkdir -p "$sp_out"
for f in \
    segmentation-3.0.onnx \
    wespeaker-voxceleb-resnet34.onnx \
    wespeaker-voxceleb-resnet34.onnx.data \
    wespeaker-voxceleb-resnet34.min_num_samples.txt \
    plda_lda.npy plda_mean1.npy plda_mean2.npy plda_mu.npy plda_psi.npy plda_tr.npy; do
    dst="$sp_out/$f"
    if [ -s "$dst" ]; then
        echo "exists: speakrs/$f ($(stat -c%s "$dst" 2>/dev/null || stat -f%z "$dst") bytes)"
        continue
    fi
    echo "fetching speakrs/$f"
    curl --fail --location --output "$dst.tmp" "$sp_base/$f"
    mv "$dst.tmp" "$dst"
done
echo "done. speakrs models at $sp_out"
ls -lh "$sp_out"
