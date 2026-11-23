// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Per-session tag suggestions ("last time" + vocab hits) for the recorder /
// summary tag pickers. Module-level cache keyed by session id — suggestions
// are read-mostly and shared across every mounted picker for the same
// session. Stays fresh two ways:
//   * `library:changed` bus — a mutation on the hook's session invalidates
//     that session's cached entry and triggers a refetch.
//   * catalog change — `useTags()` array identity flips whenever the tag
//     catalog is refetched (create/rename/delete, or any library mutation);
//     vocab-based suggestions depend on `vocab_md` per tag, so a catalog
//     change invalidates the current session's cached entry too.
// Errors are swallowed to `null` — suggestions are a quiet enhancement, not
// a feature the person has to notice failing.

import { useEffect, useRef, useState } from 'react';
import { tauri, type SessionSuggestions } from '../tauri';
import { subscribeToLibrary } from './libraryEvents';
import { useTags } from './useTags';

const cache = new Map<string, SessionSuggestions>();

async function fetchSuggestions(sessionId: string): Promise<SessionSuggestions | null> {
  try {
    const s = await tauri.tagSuggestForSession(sessionId);
    cache.set(sessionId, s);
    return s;
  } catch {
    return null;
  }
}

/** Suggestions for one session's tag picker. `null` while loading, when
 *  `sessionId` is null, or on fetch error. */
export function useTagSuggestions(sessionId: string | null): SessionSuggestions | null {
  const [suggestions, setSuggestions] = useState<SessionSuggestions | null>(
    sessionId != null ? cache.get(sessionId) ?? null : null,
  );
  const allTags = useTags();
  const prevTagsRef = useRef(allTags);

  // Fetch (or reuse the cache) for the current session; re-subscribe to the
  // library bus whenever the session changes.
  useEffect(() => {
    if (sessionId == null) {
      setSuggestions(null);
      return;
    }
    let cancelled = false;
    const cached = cache.get(sessionId);
    if (cached) {
      setSuggestions(cached);
    } else {
      setSuggestions(null);
      void fetchSuggestions(sessionId).then((s) => { if (!cancelled) setSuggestions(s); });
    }
    const unsubscribe = subscribeToLibrary((ev) => {
      // Keep the shared cache honest for every session, not just this one —
      // other mounted pickers rely on the same Map.
      cache.delete(ev.session_id);
      if (ev.session_id !== sessionId) return;
      void fetchSuggestions(sessionId).then((s) => { if (!cancelled) setSuggestions(s); });
    });
    return () => { cancelled = true; unsubscribe(); };
  }, [sessionId]);

  // Catalog edits (new/renamed/deleted tags, vocab_md changes) invalidate
  // vocab-derived suggestions for the current session.
  useEffect(() => {
    if (prevTagsRef.current === allTags) return;
    prevTagsRef.current = allTags;
    if (sessionId == null) return;
    cache.delete(sessionId);
    let cancelled = false;
    void fetchSuggestions(sessionId).then((s) => { if (!cancelled) setSuggestions(s); });
    return () => { cancelled = true; };
  }, [allTags, sessionId]);

  return suggestions;
}
