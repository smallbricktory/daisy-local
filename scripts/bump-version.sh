#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Set the app version everywhere it is written.
#
#
#
# Usage:
#   scripts/bump-version.sh 1.3.2
#
# Then commit the five files and tag, as docs/RELEASING.md describes.
set -euo pipefail

new="${1:-}"
if [ -z "$new" ]; then
  echo "usage: scripts/bump-version.sh <version>   (e.g. 1.3.2)" >&2
  exit 2
fi
case "$new" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "error: '$new' is not MAJOR.MINOR.PATCH[-suffix]" >&2; exit 2 ;;
esac

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

cargo_toml="crates/tauri-app/Cargo.toml"
tauri_conf="crates/tauri-app/tauri.conf.json"
lock="Cargo.lock"
licenses_txt="THIRD-PARTY-LICENSES.txt"
licenses_ts="apps/frontend/src/licenses.generated.ts"

old="$(sed -nE 's/^version = "([^"]+)".*/\1/p' "$cargo_toml" | head -1)"
if [ -z "$old" ]; then
  echo "error: no version in $cargo_toml" >&2
  exit 1
fi
if [ "$old" = "$new" ]; then
  echo "already at $new — nothing to do"
  exit 0
fi

# Cargo.toml + tauri.conf.json: the first version key in each.
perl -0pi -e "s/^version = \"\Q$old\E\"/version = \"$new\"/m" "$cargo_toml"
perl -0pi -e "s/\"version\": \"\Q$old\E\"/\"version\": \"$new\"/" "$tauri_conf"

awk -v old="$old" -v new="$new" '
  /^name = "tauri-app"$/ { in_pkg = 1 }
  in_pkg && $0 == "version = \"" old "\"" { print "version = \"" new "\""; in_pkg = 0; next }
  /^\[\[package\]\]$/ { in_pkg = 0 }
  { print }
' "$lock" > "$lock.tmp" && mv "$lock.tmp" "$lock"

# The generated licence files name the crate in a list of many; only the
# tauri-app entry carries this version.
perl -0pi -e "s/tauri-app \Q$old\E/tauri-app $new/g" "$licenses_txt"
perl -0pi -e "s/(\"name\": \"tauri-app\",\s*\n\s*\"version\": \")\Q$old\E(\")/\${1}$new\${2}/" "$licenses_ts"

# Every site must now read the new version and none the old one.
fail=0
for f in "$cargo_toml" "$tauri_conf" "$lock" "$licenses_txt" "$licenses_ts"; do
  if ! grep -q -- "$new" "$f"; then
    echo "error: $f still does not name $new" >&2
    fail=1
  fi
done
if grep -q "tauri-app $old" "$licenses_txt" || grep -q "\"version\": \"$old\"" "$tauri_conf"; then
  echo "error: a version site still reads $old" >&2
  fail=1
fi
[ "$fail" -eq 0 ] || exit 1

# Proves the lock still parses and matches the manifests.
cargo metadata --no-deps --offline --format-version 1 >/dev/null

echo "$old -> $new"
git --no-pager diff --stat -- "$cargo_toml" "$tauri_conf" "$lock" "$licenses_txt" "$licenses_ts"
