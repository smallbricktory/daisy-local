// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE

/** The save dialog's default name for an export of the recording `title`:
 *  letters (any script), digits, `.`, `_` and `-` kept, every other run
 *  turned into one `_`, at most 60 characters, `meeting` when nothing is
 *  left. */
export function exportFileName(title: string, ext: string): string {
  const base = Array.from(title.normalize('NFC').replace(/[^\p{L}\p{M}\p{N}._-]+/gu, '_'))
    .slice(0, 60)
    .join('')
    .replace(/_+$/, '');
  return `${base || 'meeting'}.${ext}`;
}
