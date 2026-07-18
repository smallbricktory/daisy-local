// Singleton session-lifecycle store. Mirrors recordingState.ts. The reducer
// (sessionPhase.ts) is pure; this layer wires Tauri inputs into it: the
// recording:snapshot event + a poll of the finalize.status.json sidecar
// (tauri.readFinalizeStatus) while a finalize is in flight. There is NO global
// finalize event — the default Stop path runs a detached subprocess.
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';
import { tauri, type RecordingSnapshot } from '../tauri';
import {
  reducer, initialState, type LifecycleState, type LifecycleEvent, type Phase,
} from './sessionPhase';

// Payload of the backend `library:changed` event (mirror of
// LibraryChangeEvent in libraryEvents.ts). This store listens directly, not
// via the shared bus, and owns its own listener lifecycle (and its synthetic
// visibility refresh, below).
interface LibraryChangePayload { kind?: string; session_id?: string }

let state: LifecycleState = initialState();
const subscribers = new Set<() => void>();
let unlistens: UnlistenFn[] = [];
let initialized = false;

let pollTimer: ReturnType<typeof setInterval> | null = null;
let pollSession: { sessionId: string; title: string | null } | null = null;

function notify(): void { for (const cb of subscribers) cb(); }

function dispatch(ev: LifecycleEvent): void {
  const next = reducer(state, ev);
  if (next === state) return;
  state = next;
  notify();
}

function stopPolling(): void {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  pollSession = null;
}

function startPolling(sessionId: string, title: string | null): void {
  pollSession = { sessionId, title };
  if (pollTimer) return;
  const tick = async () => {
    if (!pollSession) return;
    const s = await tauri.readFinalizeStatus(pollSession.sessionId).catch(() => null);
    if (!s) return;
    const sid = pollSession.sessionId;
    // Stale sidecar: a working stage that hasn't updated in a long time
    // means the finalize crashed/hung without reaching done/error. It is
    // marked interrupted; the toast offers Resume. `awaiting-labels` is an
    // indefinite pause (waiting on the user) and is exempt.
    const STALE_SECS = 300;
    const ageSecs = Date.now() / 1000 - s.updated_at_unix;
    const exempt = s.stage === 'done' || s.stage === 'error' || s.stage === 'awaiting-labels';
    if (!exempt && ageSecs > STALE_SECS) {
      dispatch({ type: 'finalize', sessionId: sid, title: pollSession.title,
        progress: { stage: 'error', progress: s.progress, message: s.message } });
      stopPolling();
      return;
    }
    dispatch({ type: 'finalize', sessionId: sid, title: pollSession.title,
      progress: { stage: s.stage, progress: s.progress, message: s.message } });
    if (s.stage === 'done' || s.stage === 'error') stopPolling();
    // At the terminal `done` stage the sidecar carries no AI-summary /
    // speaker-count info — the reducer fills those with placeholders.
    // Best-effort enrichment reads the real session. A failure here never
    // breaks the poll: it degrades silently to the placeholder (hadAi:false).
    if (s.stage === 'done') void enrichDone(sid);
  };
  pollTimer = setInterval(() => { void tick(); }, 1000);
  void tick();
}

/** Best-effort enrichment of the terminal `done` phase. The finalize sidecar
 *  only knows the stage reached, not whether an AI summary was produced or
 *  how many speakers were identified; the reducer's `done` placeholder reads
 *  "no AI summary". Pulls the truth from the session and dispatches
 *  `enrich-done` (a no-op in the reducer unless the current phase is still
 *  `done` for this session). Never throws — a fetch failure leaves the
 *  hadAi:false placeholder. */
async function enrichDone(sessionId: string): Promise<void> {
  const [summary, speakers] = await Promise.all([
    tauri.summaryLoad(sessionId).catch(() => null),
    tauri.listSessionSpeakers(sessionId).catch(() => [] as { cluster_id: number }[]),
  ]);
  dispatch({
    type: 'enrich-done',
    sessionId,
    summary: { hadAi: summary != null, speakers: speakers.length || null, durationLabel: null },
  });
}

/** Starts watching a session's finalize progress on demand. Used by the
 *  manual re-summarize / regen paths (App.tsx), which kick a detached
 *  finalize without going through the Stop snapshot transition and arm the
 *  poll themselves. */
export function beginFinalizeWatch(sessionId: string, title: string | null): void {
  startPolling(sessionId, title);
}

async function attach(): Promise<void> {
  if (unlistens.length) return;
  unlistens.push(await listen<RecordingSnapshot | null>('recording:snapshot', (e) => {
    // Capture the recording session id (if any) BEFORE dispatch flips the phase.
    const recId = state.phase.kind === 'recording' ? state.phase.snap.session_id : null;
    dispatch({ type: 'snapshot', snap: e.payload });
    // Stop transition: snapshot went null while we were recording → a finalize
    // is now in flight (detached). Begin polling the sidecar for that session.
    if (e.payload == null && recId) startPolling(recId, null);
  }));
  unlistens.push(await listen<LibraryChangePayload>('library:changed', (e) => {
    // Detached-finalize completion wakes us via library:changed. Poll the
    // named session's sidecar to pick up the terminal stage.
    const sid = e.payload?.session_id;
    if (sid) startPolling(sid, pollSession?.title ?? null);
  }));
}

async function coldFetch(): Promise<void> {
  try { dispatch({ type: 'snapshot', snap: await tauri.recordingSnapshot() }); }
  catch { /* backend not ready */ }
}

async function ensureInitialized(): Promise<void> {
  if (initialized) return;
  initialized = true;
  await attach();
  await coldFetch();
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') void coldFetch();
  });
}

export function dismissPhase(): void { dispatch({ type: 'dismiss' }); }

export function useSessionPhase(): Phase {
  const [, setTick] = useState(0);
  useEffect(() => {
    void ensureInitialized();
    const cb = () => setTick((n) => n + 1);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return state.phase;
}

export const __testStore = {
  reset(): void { state = initialState(); subscribers.clear(); stopPolling(); },
  dispatch(ev: LifecycleEvent): void { dispatch(ev); },
  getPhase(): Phase { return state.phase; },
  subscribe(cb: () => void): () => void { subscribers.add(cb); return () => { subscribers.delete(cb); }; },
};
