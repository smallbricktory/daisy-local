#!/bin/bash
# SPDX-License-Identifier: Elastic-2.0
# Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
# Every data-testid in the frontend, and whether a spec, page object or vitest
# names it. A literal match is necessary, not sufficient: an id a page object
# builds from a slug (`session-action-${slug}`) is reported by its suffix.
set -euo pipefail
cd "$(dirname "$0")/../apps/frontend"
ids=$(mktemp) all=$(mktemp) unc=$(mktemp)
trap 'rm -f "$ids" "$all" "$unc"' EXIT
grep -rho 'data-testid="[^"]*"' src | sed 's/data-testid="//;s/"//' | sort -u > "$ids"
cat e2e/specs/*.ts e2e/pages/*.ts > "$all"
find src \( -name '*.test.tsx' -o -name '*.test.ts' \) -exec cat {} + >> "$all" 2>/dev/null || true
while IFS= read -r id; do
  hit=""
  grep -qF -- "$id" "$all" && hit=1
  if [ -z "$hit" ]; then
    for n in 1 2 3; do
      suf=$(echo "$id" | rev | cut -d- -f1-"$n" | rev)
      if grep -qF -- "'$suf'" "$all" || grep -qF -- "\"$suf\"" "$all"; then hit=1; break; fi
    done
  fi
  [ -z "$hit" ] && echo "$id" >> "$unc"
done < "$ids"
printf '%s test ids; %s named by no spec, page object or vitest\n' \
  "$(wc -l < "$ids" | tr -d ' ')" "$(wc -l < "$unc" 2>/dev/null | tr -d ' ' || echo 0)"
[ -s "$unc" ] && cat "$unc"
exit 0
