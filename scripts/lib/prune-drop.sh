# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# prune_drop DIR SHA PLATFORM...: deletes each named platform's installers
# in DIR whose name lacks SHA. Platforms: linux, win, mac. A platform not
# named keeps every installer it has there.
prune_drop() {
  local dir=$1 sha=$2
  shift 2
  local platform p f
  for platform in "$@"; do
    local -a pats=()
    case "$platform" in
      linux) pats=('Daisy_*_amd64.AppImage' 'Daisy_*_amd64.deb') ;;
      win) pats=('Daisy_*_x64-setup.exe' 'daisy-*.zip') ;;
      mac) pats=('Daisy_*_aarch64.dmg') ;;
      *) echo "prune_drop: unknown platform $platform" >&2; return 2 ;;
    esac
    for p in "${pats[@]}"; do
      for f in "$dir"/$p; do
        [ -e "$f" ] || continue
        case "$(basename "$f")" in
          *"$sha"*) ;;
          *) rm -f "$f"; echo "pruned $(basename "$f")" ;;
        esac
      done
    done
  done
}
