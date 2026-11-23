#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Checks prune_drop: a platform that built gives up its older installers,
# one that did not keeps them, and the new build's files stay.
set -euo pipefail
source "$(cd "$(dirname "$0")" && pwd)/lib/prune-drop.sh"
d=$(mktemp -d)
trap 'rm -rf "$d"' EXIT
old=aaaa1111 new=bbbb2222
for s in $old $new; do
  touch "$d/Daisy_1.0.0_${s}_amd64.AppImage" "$d/Daisy_1.0.0_${s}_amd64.deb" \
        "$d/Daisy_1.0.0_${s}_x64-setup.exe" "$d/daisy-1.0.0-${s}.zip" "$d/Daisy_1.0.0_${s}_aarch64.dmg"
done
touch "$d/BUILD-INFO.txt"
prune_drop "$d" "$new" linux mac >/dev/null
fail=0
expect() { if [ "$1" = gone ] && [ -e "$d/$2" ]; then echo "FAIL: $2 should be pruned"; fail=1; fi
           if [ "$1" = kept ] && [ ! -e "$d/$2" ]; then echo "FAIL: $2 should be kept"; fail=1; fi; }
expect gone "Daisy_1.0.0_${old}_amd64.AppImage"
expect gone "Daisy_1.0.0_${old}_amd64.deb"
expect gone "Daisy_1.0.0_${old}_aarch64.dmg"
expect kept "Daisy_1.0.0_${old}_x64-setup.exe"
expect kept "daisy-1.0.0-${old}.zip"
for f in "Daisy_1.0.0_${new}_amd64.AppImage" "Daisy_1.0.0_${new}_amd64.deb" "Daisy_1.0.0_${new}_x64-setup.exe" \
         "daisy-1.0.0-${new}.zip" "Daisy_1.0.0_${new}_aarch64.dmg" BUILD-INFO.txt; do expect kept "$f"; done
prune_drop "$d" "$new" >/dev/null
expect kept "Daisy_1.0.0_${old}_x64-setup.exe"
if prune_drop "$d" "$new" ios 2>/dev/null; then echo "FAIL: an unknown platform is an error"; fail=1; fi
[ $fail -eq 0 ] && echo "prune_drop: ok"
exit $fail
