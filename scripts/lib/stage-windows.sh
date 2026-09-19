# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Shared staging of this repo to the Windows side of the interop, sourced by
# every script that runs cargo/powershell over there. Defines the paths and
# one function:
#
#   stage_windows   rsync the tree to $STAGE (C:\tmp\daisy-build), failing if
#                   interop is missing. $STAGE/target is a warm cache shared
#                   by all callers and is deliberately not cleaned.
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAGE=/mnt/c/tmp/daisy-build
PS=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
RSYNC_EXCLUDES=(--exclude target --exclude node_modules --exclude .git
  --exclude 'apps/frontend/dist' --exclude .vite --exclude '*.dmg'
  --exclude .pnpm-store --exclude .remember --exclude diagnostics-out)

stage_windows() {
  [ -x "$PS" ] || { echo "no Windows interop on this host — cannot run" >&2; return 1; }
  # 24 = "some files vanished before transfer" — tolerated.
  rsync -a --delete "${RSYNC_EXCLUDES[@]}" "$REPO/" "$STAGE/"
  local rc=$?
  [ "$rc" -eq 0 ] || [ "$rc" -eq 24 ] || return 1
  return 0
}
