#!/usr/bin/env bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
#
# Fails when a shipping executable was built against the license test stack.
# Usage: check-no-license-test.sh <executable>...
# Reads scripts/license-test-markers.txt: each `+` string must be present and
# each `-` string absent. Scan raw executables.
set -euo pipefail

markers="$(dirname "$0")/license-test-markers.txt"
[ $# -gt 0 ] || { echo "usage: $0 <executable>..." >&2; exit 2; }

fail=0
for bin in "$@"; do
    [ -f "$bin" ] || { echo "FATAL: $bin not found" >&2; exit 1; }
    while IFS= read -r line; do
        case "$line" in
            +*)
                if ! LC_ALL=C grep -aqF -- "${line#+}" "$bin"; then
                    echo "FATAL: $bin lacks the production license key — wrong file, or a license-test build" >&2
                    fail=1
                fi
                ;;
            -*)
                if LC_ALL=C grep -aqF -- "${line#-}" "$bin"; then
                    echo "FATAL: $bin contains license test-stack marker ${line#-}" >&2
                    fail=1
                fi
                ;;
        esac
    done < "$markers"
done
[ "$fail" -eq 0 ] || exit 1
echo "license guard: $* clean"
