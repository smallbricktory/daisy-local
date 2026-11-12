#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Emit a CycloneDX SBOM for the shipped app: the tauri-app crate graph from
# cargo-cyclonedx plus the bundled model weights, which live in no package
# graph (hand-maintained in scripts/lib/model-sbom.json, mirroring
# scripts/lib/model-notices.txt).
#
#   scripts/gen-sbom.sh [out.json]      default: dist/daisy-<version>-sbom.json
#
# Needs cargo-cyclonedx: cargo install --locked cargo-cyclonedx

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

command -v cargo-cyclonedx >/dev/null \
  || { echo "(error) cargo-cyclonedx not installed — cargo install --locked cargo-cyclonedx" >&2; exit 1; }

version="$(grep -m1 '^version' crates/tauri-app/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
out="${1:-dist/daisy-${version}-sbom.json}"

tmp="$(mktemp -d)"
# cargo-cyclonedx writes one sbom.json next to every workspace member's
# Cargo.toml regardless of --manifest-path; only tauri-app's is kept.
trap 'rm -rf "$tmp"; find "$repo_root/crates" -maxdepth 2 -name sbom.json -delete' EXIT

# --all: the full dependency graph, not just direct deps. 1.5 is the newest
# spec cargo-cyclonedx emits and the one that has a machine-learning-model
# component type for the model entries merged below.
cargo cyclonedx \
  --manifest-path crates/tauri-app/Cargo.toml \
  --format json --all --spec-version 1.5 \
  --override-filename sbom >/dev/null

crate_bom="crates/tauri-app/sbom.json"
[ -f "$crate_bom" ] || { echo "(error) cargo-cyclonedx wrote no $crate_bom" >&2; exit 1; }
mv "$crate_bom" "$tmp/crates.json"

mkdir -p "$(dirname "$out")"
python3 - "$tmp/crates.json" scripts/lib/model-sbom.json "$out" <<'PY'
import json
import sys

bom_path, models_path, out_path = sys.argv[1:4]
with open(bom_path, encoding="utf-8") as fh:
    bom = json.load(fh)
with open(models_path, encoding="utf-8") as fh:
    models = json.load(fh)

bom.setdefault("components", []).extend(models)
with open(out_path, "w", encoding="utf-8") as fh:
    json.dump(bom, fh, indent=2)
    fh.write("\n")
print(f"wrote {out_path} ({len(bom['components'])} components, {len(models)} of them models)")
PY
