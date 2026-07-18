import { useCallback, useEffect, useRef, useState } from 'react';
import {
  tauri,
  errStr,
  type SessionView,
  type SessionMeta,
  type SessionSummary,
  type SessionChapters,
  type SessionSpeaker,
  type Tag,
} from '../tauri';
import { subscribeToLibrary } from './libraryEvents';

export interface SessionData {
  view: SessionView | null;
  meta: SessionMeta | null;
  summary: SessionSummary | null;
  chapters: SessionChapters | null;
  speakers: SessionSpeaker[] | null;
  tags: Tag[];
  loadErr: string | null;
  /** Manual reload (e.g. after a local-only edit). */
  reload: () => Promise<void>;
}

/**
 * Own "load one session's data + stay fresh". Fetches the full set in one go
 * (cheap local flat-file reads), then refetches whenever `library:changed`
 * fires for this session id — or for the synthetic stale signal (empty id,
 * emitted on visibility-resume).
 *
 * Concurrency: every reload bumps a generation counter and only the LATEST
 * reload may write state. This makes overlapping reloads safe in both
 * directions — a same-session burst collapses to the last write, and switching
 * sessions while a fetch is in flight can never let the old session's result
 * clobber the new one (the stale generation is dropped). Unlike an in-flight
 * gate, it never blocks a newer reload from starting.
 */
export function useSessionData(sessionId: string): SessionData {
  const [view, setView] = useState<SessionView | null>(null);
  const [meta, setMeta] = useState<SessionMeta | null>(null);
  const [summary, setSummary] = useState<SessionSummary | null>(null);
  const [chapters, setChapters] = useState<SessionChapters | null>(null);
  const [speakers, setSpeakers] = useState<SessionSpeaker[] | null>(null);
  const [tags, setTags] = useState<Tag[]>([]);
  const [loadErr, setLoadErr] = useState<string | null>(null);

  const generation = useRef(0);

  const reload = useCallback(async (): Promise<void> => {
    const gen = ++generation.current;
    setLoadErr(null);
    try {
      const [v, m, s, t, c, sp] = await Promise.all([
        tauri.readSession(sessionId),
        tauri.sessionMetaGet(sessionId),
        tauri.summaryLoad(sessionId),
        tauri.listTags(),
        tauri.loadSessionChapters(sessionId).catch(() => null),
        tauri.listSessionSpeakers(sessionId).catch(() => [] as SessionSpeaker[]),
      ]);
      // A newer reload (same-session burst, or a session switch) superseded
      // this one — drop the stale result.
      if (gen !== generation.current) return;
      setView(v);
      setMeta(m);
      setSummary(s);
      setTags(t);
      setChapters(c);
      setSpeakers(sp);
    } catch (e) {
      if (gen !== generation.current) return;
      setLoadErr(errStr(e));
    }
  }, [sessionId]);

  // Initial load + reload on sessionId change. Derived state is reset; a
  // stale session's data never flashes under a new id.
  useEffect(() => {
    setView(null); setMeta(null); setSummary(null);
    setChapters(null); setSpeakers(null);
    void reload();
  }, [reload]);

  // Stay fresh: refetch on a library:changed for THIS session, or on the
  // synthetic stale signal (session_id === '').
  useEffect(() => {
    return subscribeToLibrary((ev) => {
      if (ev.session_id === sessionId || ev.session_id === '') void reload();
    });
  }, [sessionId, reload]);

  return { view, meta, summary, chapters, speakers, tags, loadErr, reload };
}
