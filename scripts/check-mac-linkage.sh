#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Fails when an executable in a macOS .app bundle links a library outside
# the OS or the bundle itself (e.g. a Homebrew dylib on the build machine).
#
#   scripts/check-mac-linkage.sh <path/to/Daisy.app>
set -euo pipefail

APP="${1:?usage: check-mac-linkage.sh <app bundle>}"
[ -d "$APP/Contents/MacOS" ] || { echo "FATAL: $APP has no Contents/MacOS" >&2; exit 1; }

bad=0
checked=0
while IFS= read -r -d '' f; do
  file -b "$f" | grep -q 'Mach-O' || continue
  checked=$((checked + 1))
  # Lines ending in ':' name the file (one per slice of a universal binary).
  while read -r lib _; do
    case "$lib" in
      *:) ;;
      /System/Library/*|/usr/lib/*|@rpath/*|@executable_path/*|@loader_path/*) ;;
      *) echo "FATAL: $(basename "$f") links $lib" >&2; bad=1 ;;
    esac
  done < <(otool -L "$f")
  # @rpath references resolve through LC_RPATH: only the OS Swift runtime
  # and the bundle itself may appear there.
  while read -r rp; do
    case "$rp" in
      /usr/lib/swift|/usr/lib/swift/*|@executable_path/*|@loader_path/*) ;;
      *) echo "FATAL: $(basename "$f") has rpath $rp" >&2; bad=1 ;;
    esac
  done < <(otool -l "$f" | awk '/cmd LC_RPATH/ {r=1; next} r && $1 == "path" {print $2; r=0}')
done < <(find "$APP/Contents/MacOS" "$APP/Contents/Frameworks" -type f -print0 2>/dev/null)

[ "$checked" -gt 0 ] || { echo "FATAL: no Mach-O executables found in $APP" >&2; exit 1; }
[ "$bad" -eq 0 ] || exit 1
echo "linkage: $checked executable(s) in $(basename "$APP") link only system and bundle libraries"
