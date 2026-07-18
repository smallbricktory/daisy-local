#!/usr/bin/env bash
# Fetch the bge-small-en-v1.5 ONNX export + tokenizer for the Q&A retriever.
# Idempotent: skips files
# already on disk. Stores under models/embeddings/.
#
# License: MIT (BAAI). Re-distributable; safe to bundle into the AppImage.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/embeddings"
mkdir -p "$out"

# Xenova/ converts each BAAI release into the standard transformers.js ONNX
# layout (model.onnx + tokenizer.json + config.json), which is what `ort` +
# `tokenizers` consume directly.
base="https://huggingface.co/Xenova/bge-small-en-v1.5/resolve/main"

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

rm -f "$out/model.onnx.tmp"
# The int8-quantized export (≈33 MB) is fetched, not the fp32 (≈127 MB).
fetch "onnx/model_quantized.onnx" "model.onnx"
fetch "tokenizer.json"            "tokenizer.json"
fetch "config.json"               "config.json"

echo "done. embeddings model at $out"
ls -lh "$out"
