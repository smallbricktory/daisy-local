#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fetch the Nemotron streaming ASR model (Windows/Linux engine) into
# models/nemotron/<id>/. Idempotent: files whose sha256 already matches are
# kept. Pins mirror crates/providers-local/src/registry.rs (NEMOTRON_*);
# crates/providers-local/tests/packaging_pins.rs asserts they stay equal.
#
# The files are Daisy's own ONNX export (see scripts/nemotron-export/),
# published as release assets on smallbricktory/daisy-local.
#
# License: NVIDIA Open Model License. Bundled into the Windows and Linux installers.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/nemotron/nemotron-speech-streaming-en-0.6b-560ms-int8"
mkdir -p "$out"

base="https://github.com/smallbricktory/daisy-local/releases/download/asr-nemotron-streaming-en-0.6b-560ms-int8-1"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

fetch() {
    local name="$1"
    local want="$2"
    local dst="$out/$name"
    if [ "$(sha256_of "$dst" 2>/dev/null)" = "$want" ]; then
        echo "exists: $name"
        return 0
    fi
    echo "fetching $name -> $dst"
    curl --fail --location --output "$dst.tmp" "$base/$name"
    local got
    got="$(sha256_of "$dst.tmp")"
    if [ "$got" != "$want" ]; then
        rm -f "$dst.tmp"
        echo "sha256 mismatch for $name: got $got, want $want" >&2
        exit 1
    fi
    mv "$dst.tmp" "$dst"
}

fetch "encoder.int8.onnx"      "48fdad330d1143f05940d43d4a090c14b18e3dfb98b7b4c430f5536df531934d"
fetch "encoder.int8.onnx.data" "7dc691f551ce9fc01ac97a87c8f72c85e9a348ab310ee57c033540b613915a12"
fetch "decoder.onnx"           "809ad04d35a38030150c11f5fe982c319fc27cf4c703c2421eea49dca05e02fd"
fetch "joiner.onnx"            "e7d0168dc1c76718c54d2afb63ee5ee1b68658cb68a5baf11b79de40378663a4"
fetch "tokens.txt"             "dc0b4584ab2e4ddbf888425c076c61b736e7356a015250db7d307e6f1a8188ff"

echo "done. nemotron model at $out"
