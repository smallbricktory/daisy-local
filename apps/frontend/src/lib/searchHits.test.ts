// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, expect, it } from 'vitest';
import { hitSnippets } from './searchHits';
import type { SessionHit } from '../tauri';

function hit(partial: Partial<SessionHit>): SessionHit {
  return {
    session_id: 's1',
    title: 'Pilot scope review',
    created_at_unix_seconds: 0,
    match_source: 'title',
    matches: [],
    ...partial,
  } as SessionHit;
}

describe('hitSnippets', () => {
  it('drops a snippet that repeats the row title', () => {
    const h = hit({ matches: [{ source: 'title', snippet: 'Pilot scope review' }] as SessionHit['matches'] });
    expect(hitSnippets(h)).toEqual([]);
  });

  it('ignores case and surrounding space when comparing', () => {
    const h = hit({ matches: [{ source: 'title', snippet: '  pilot SCOPE review ' }] as SessionHit['matches'] });
    expect(hitSnippets(h)).toEqual([]);
  });

  it('falls back to the session id when the title is empty', () => {
    const h = hit({ title: '', matches: [{ source: 'title', snippet: 's1' }] as SessionHit['matches'] });
    expect(hitSnippets(h)).toEqual([]);
  });

  it('keeps snippets that say something the title does not', () => {
    const m = [{ source: 'transcript', snippet: 'we ship on May 12' }] as SessionHit['matches'];
    expect(hitSnippets(hit({ matches: m }))).toEqual(m);
  });
});
