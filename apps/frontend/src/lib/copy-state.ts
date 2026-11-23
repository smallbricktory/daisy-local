// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
const COPY_LARGE_BYTES = 5 * 1024 * 1024;

export function copyOutcomeForSize(byteLength: number, threshold = COPY_LARGE_BYTES): 'copied' | 'truncated' {
  return byteLength > threshold ? 'truncated' : 'copied';
}
