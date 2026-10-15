// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE

const CHANGELOG = 'https://www.daisylocal.app/changelog';

/** The changelog page anchored at a release: `1.3.3-a` opens `#1.3.3`.
 *  A version with no MAJOR.MINOR.PATCH opens the page unanchored. */
export function releaseNotesUrl(version: string): string {
  const core = /^v?(\d+\.\d+\.\d+)/.exec(version.trim());
  return core ? `${CHANGELOG}#${core[1]}` : CHANGELOG;
}
