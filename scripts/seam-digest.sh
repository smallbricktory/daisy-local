#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
#
# Prints "<sha256>  <relative/path>" for every regular file under the given
# path — one line per file, sorted by path — or, given a single file, one
# line for that file. This is the manifest format committed alongside the
# sync-seam corpus (tests/fixtures/sync-seam.sha256) and the sync vectors
# (tests/fixtures/sync-vectors-v1.sha256): a fixture change then has to touch
# a file whose diff review can see.
#
# Byte-for-byte identical to the copy of this script in smbr-daisy-ios. Run
# against each repo's own fixture path, the two outputs match exactly
# whenever the fixtures do — that is the seam's byte-identity check.
#
# Usage:
#   scripts/seam-digest.sh <file-or-directory>
set -euo pipefail

target="${1:?usage: seam-digest.sh <file-or-directory>}"

if [ -f "$target" ]; then
  dir="$(dirname "$target")"
  base="$(basename "$target")"
  (cd "$dir" && shasum -a 256 "$base")
elif [ -d "$target" ]; then
  (cd "$target" && find . -type f | sed 's#^\./##' | LC_ALL=C sort | xargs shasum -a 256)
else
  echo "seam-digest.sh: no such file or directory: $target" >&2
  exit 1
fi
