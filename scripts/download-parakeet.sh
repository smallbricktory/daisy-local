#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Stage the bundled parakeet model (win/linux floor row): fetch the upstream
# release tarball, verify it, extract the int8 file set into
# models/parakeet/<id>/ with per-file SHA256 checks. Pins mirror
# crates/providers-local/src/registry.rs (the parakeet ModelRow) —
# crates/providers-local/tests/packaging_pins.rs asserts they stay equal.
# Idempotent.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
id="parakeet-tdt-0.6b-v2-int8"
out="$repo_root/models/parakeet/$id"

tar_url="https://github.com/jason-ni/parakeet-rs/releases/download/v0.1.0/parakeet-tdt-0.6b-v2-onnx.tar.gz"
tar_sha256="ea3dfcc0a5f98be157754a29e05db129deece29b260f33599de4a5af59952b2c"
tar_root="parakeet-tdt-0.6b-v2"

# file  sha256 — keep in lock-step with registry.rs.
pins="\
encoder.int8.onnx a32b12d17bbbc309d0686fbbcc2987b5e9b8333a7da83fa6b089f0a2acd651ab
decoder.int8.onnx b6bb64963457237b900e496ee9994b59294526439fbcc1fecf705b31a15c6b4e
joint.pred.int8.onnx 460003416f807eba35c248f0908d4d8377c57a9909709362b1bcfa6b86b17c8e
joint.enc.int8.onnx 46399f4ac2c5a09199980a0c15a14bc4961a21adc613e8af4fc9929c5e074ce4
joint.joint_net.int8.onnx 4be698e206ac2b5860fbcedde08f64db9012a415acd9ecdde80f589e0cf34f94
nemo128.onnx e1f0c3c95be11f19026ff341b946aba9b8ca51ba3c2268de91466bf3336ffff7
tokens.txt ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d
model_desc.json 5a4d575dd6557a03b3d1fd1db4700454a056b1b410bde5e350beb76a2548a439
"

verify_all() {
    local dir="$1"
    while read -r name want; do
        [ -n "$name" ] || continue
        local f="$dir/$name"
        [ -f "$f" ] || { echo "(missing) $f" >&2; return 1; }
        echo "$want  $f" | sha256sum -c - >/dev/null 2>&1 \
            || { echo "(sha mismatch) $f" >&2; return 1; }
    done <<< "$pins"
}

if [ -d "$out" ] && verify_all "$out"; then
    echo "parakeet: $id present + verified ($(du -sh "$out" | cut -f1))"
    exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "parakeet: fetching $tar_url"
curl --fail --location --output "$tmp/model.tar.gz" "$tar_url"
echo "$tar_sha256  $tmp/model.tar.gz" | sha256sum -c - \
    || { echo "parakeet tarball SHA mismatch — upstream changed" >&2; exit 1; }

# Extract only the pinned files (the tarball also carries fp32 weights).
while read -r name _; do
    [ -n "$name" ] || continue
    tar -xzf "$tmp/model.tar.gz" -C "$tmp" "$tar_root/$name"
done <<< "$pins"

verify_all "$tmp/$tar_root"

mkdir -p "$(dirname "$out")"
rm -rf "$out"
mv "$tmp/$tar_root" "$out"
echo "parakeet: staged $out ($(du -sh "$out" | cut -f1))"
