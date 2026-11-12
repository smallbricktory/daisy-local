#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fetch the granite-embedding-97m-multilingual-r2 ONNX model + tokenizer for
# search and Ask. Idempotent: files whose sha256 already matches are kept.
# Stores under models/embeddings/.
#
# model.onnx is Daisy's own export (int8 embedding table, fp16 weights),
# published as a release asset on smallbricktory/daisy-local.
#
# License: Apache-2.0 (IBM). Re-distributable; bundled into every installer.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/embeddings"
mkdir -p "$out"

base="https://github.com/smallbricktory/daisy-local/releases/download/embeddings-granite-g8m16-1"

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
        echo "exists: $name ($(stat -c%s "$dst" 2>/dev/null || stat -f%z "$dst") bytes)"
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

fetch "model.onnx"     "38e1c4aa1d4fa12ab26be368c28a30e7abefd9a4c0c305cc97c6502b6f753763"
fetch "tokenizer.json" "4f2842d568e2724370aec203652a42ac783c7937f8347a1a2cc7506d71f1582f"
fetch "config.json"    "de948b0bdc6f356afad7a84b276d8dd7e7fe10fb9add1bb5e610621c28e41ebc"

echo "done. embeddings model at $out"
ls -lh "$out"
