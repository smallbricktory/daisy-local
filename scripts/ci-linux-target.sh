#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Points the Linux CI jobs at a cargo target dir that survives between runs:
# $HOME/ci-target, outside the checkout that actions/checkout cleans. Over
# CAP_GB it is wiped first and the run builds cold. Incremental compilation is
# off and dev/test builds carry line tables only. Run as a step; it writes
# CARGO_TARGET_DIR, CARGO_INCREMENTAL and CARGO_PROFILE_DEV_DEBUG to
# $GITHUB_ENV for the steps after it.
set -euo pipefail
CAP_GB=60
dir="$HOME/ci-target"
mkdir -p "$dir"
used_gb=$(( $(du -sk "$dir" | cut -f1) / 1024 / 1024 ))
if (( used_gb > CAP_GB )); then
  echo "ci-target holds ${used_gb} GB (cap ${CAP_GB}); wiping it"
  rm -rf "$dir"
  mkdir -p "$dir"
else
  echo "ci-target holds ${used_gb} GB (cap ${CAP_GB})"
fi
echo "CARGO_TARGET_DIR=$dir" >> "$GITHUB_ENV"
echo "CARGO_INCREMENTAL=0" >> "$GITHUB_ENV"
echo "CARGO_PROFILE_DEV_DEBUG=line-tables-only" >> "$GITHUB_ENV"
