#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Publish the Nemotron model files as release assets on smallbricktory/daisy-local.
# usage: scripts/publish-nemotron-model.sh <dir-with-the-five-files>
#
# Idempotent: creates the release when absent, uploads only assets the release
# lacks. Verifies every file against scripts/nemotron-export/SHA256SUMS before
# upload, then downloads each asset back and verifies it again.
# Needs `gh auth status` to succeed with write access to the repo.

set -euo pipefail

repo="smallbricktory/daisy-local"
tag="asr-nemotron-streaming-en-0.6b-560ms-int8-1"
here="$(cd "$(dirname "$0")" && pwd)"
sums="$here/nemotron-export/SHA256SUMS"
src="${1:?usage: $0 <dir-with-the-five-files>}"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

verify_dir() {
    local dir="$1" want name got
    while read -r want name; do
        [ -f "$dir/$name" ] || { echo "missing: $dir/$name" >&2; exit 1; }
        got="$(sha256_of "$dir/$name")"
        if [ "$got" != "$want" ]; then
            echo "sha256 mismatch for $name: got $got, want $want" >&2
            exit 1
        fi
        echo "ok: $name"
    done < "$sums"
}

echo "== verifying local files"
verify_dir "$src"

notes="$(mktemp)"
back="$(mktemp -d)"
trap 'rm -rf "$notes" "$back"' EXIT
cat > "$notes" <<'EOF'
NVIDIA Nemotron Speech Streaming 0.6B (English, 560 ms chunks), ONNX export by Small Bricktory.

Licensed by NVIDIA Corporation under the NVIDIA Open Model License: https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/

Model: https://huggingface.co/nvidia/nemotron-speech-streaming-en-0.6b at revision ac0580bb7d3d6e39c4361db6afe28db9211793e4
Export: sherpa-onnx scripts/nemo/nemotron-speech-streaming-en-0.6b/export_onnx.py at commit 415b8c2aa261bcbe6c88772db9d80350955d1502 (Apache-2.0), adapted; encoder weights quantized to per-channel uint8 (MatMul, Gemm and Conv except the subsampling stack); decoder and joiner stay fp32. Reproduction recipe and sha256 list: scripts/nemotron-export/ in the Daisy repository.
EOF

if gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
    echo "== release $tag exists"
else
    echo "== creating release $tag"
    gh release create "$tag" --repo "$repo" \
        --title "Nemotron Speech Streaming 0.6B int8 (560 ms)" --notes-file "$notes"
fi

have="$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name')"
while read -r _ name; do
    if printf '%s\n' "$have" | grep -qx "$name"; then
        echo "asset present: $name"
    else
        echo "uploading $name"
        gh release upload "$tag" --repo "$repo" "$src/$name"
    fi
done < "$sums"

echo "== verifying published assets"
while read -r _ name; do
    gh release download "$tag" --repo "$repo" --pattern "$name" --dir "$back" --clobber
done < "$sums"
verify_dir "$back"

echo "done. https://github.com/$repo/releases/tag/$tag"
