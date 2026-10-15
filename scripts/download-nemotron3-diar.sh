#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fetch the Nemotron 3 Diarization fast32 model (live speaker names) into
# models/nemotron3-diar/. Idempotent: files whose sha256 already matches are
# kept. Ships in every desktop artifact (macOS, Windows, Linux).
#
# The files are Daisy's own ONNX export of nvidia/Nemotron-3-Diarization at
# revision f667ed73aee57d40cc39428eb768b4fd87a0a29e (scripts/nemotron3-diar/),
# published as release assets on smallbricktory/daisy-local. The release's
# SHA256SUMS.txt lists these same values; the low graph is not bundled.
# crates/providers-local/tests/packaging_pins.rs asserts the file set here
# equals the tauri bundle resources and the file names in nemotron3_diar.rs.
#
# License: OpenMDW-1.1 (LICENSE-OpenMDW-1.1.txt and NOTICE ship beside the model).

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
out="$repo_root/models/nemotron3-diar"
mkdir -p "$out"

base="https://github.com/smallbricktory/daisy-local/releases/download/diarizer-nemotron3-ga-2026-09-23-1"

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

fetch "nemotron3_diar_fast32.onnx" "8ddce2b99b08495a66730f4c2de9d06fd0b23b72d2ab8888f3508b3f665090ca"
fetch "learnable_sil_emb.bin"      "d4417b3c0eabdf7c47032fac2b5b5a7ee83d819a6ddda8fd8eaf74e2b5cc4ac7"
fetch "nemotron3_diar.json"        "50eb7469e743bc6d3ec92183436fa62f603d9bae59edba6c916987e5e7f314d5"
fetch "NOTICE"                     "1fb96eb65f7f8be54c4cb6e81d4fbc53a2564de8df4d0372eb930d314fbefd6c"
fetch "LICENSE-OpenMDW-1.1.txt"    "2ab44b68365473c112f5092211a38f231cb23e50de68b75a13369adbd76a74df"

echo "done. nemotron3 diarization model at $out"
