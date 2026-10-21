// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { MatchSnippet, SessionHit } from '../tauri';

/** The snippets a result row shows under its title. A title match snippets the
 *  title itself, which the row already prints above — those drop out. */
export function hitSnippets(hit: SessionHit): MatchSnippet[] {
  const title = (hit.title || hit.session_id).trim().toLowerCase();
  return hit.matches.filter((m) => m.snippet.trim().toLowerCase() !== title);
}
