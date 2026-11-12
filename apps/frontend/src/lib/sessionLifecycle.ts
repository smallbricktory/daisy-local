// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Singleton session-lifecycle store. The reducer (sessionPhase.ts) is pure;
// this layer chains off the shared stores: the recordingState snapshot store
// (which owns the `recording:snapshot` listener, cold fetch and visibility
// catch-up) and the libraryEvents bus, plus a poll of the
// finalize.status.json sidecar (tauri.readFinalizeStatus) while a finalize is
// in flight.
import { useEffect, useState } from 'react';
import { tauri } from '../tauri';
import { subscribeToLibrary } from './libraryEvents';
import { getRecordingSnapshot, subscribeRecordingSnapshot } from './recordingState';
import {
  reducer, initialState, type LifecycleState, type LifecycleEvent, type Phase,
} from './sessionPhase';

let state: LifecycleState = initialState();
const subscribers = new Set<() => void>();
let initialized = false;

let pollTimer: ReturnType<typeof setInterval> | null = null;
let pollSession: { sessionId: string; title: string | null } | null = null;
// Sessions whose finalize this window started watching.
const watched = new Set<string>();

function notify(): void { for (const cb of subscribers) cb(); }

function dispatch(ev: LifecycleEvent): void {
  const next = reducer(state, ev);
  if (next === state) return;
  state = next;
  notify();
}

function stopPolling(): void {
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  if (pollSession) watched.delete(pollSession.sessionId);
  pollSession = null;
}

function startPolling(sessionId: string, title: string | null): void {
  pollSession = { sessionId, title };
  watched.add(sessionId);
  if (pollTimer) return;
  const tick = async () => {
    if (!pollSession) return;
    const s = await tauri.readFinalizeStatus(pollSession.sessionId).catch(() => null);
    if (!s) return;
    const sid = pollSession.sessionId;
    // A working stage that has not updated within the stale window is marked
    // interrupted; the toast offers Resume. `awaiting-labels` is exempt.
    const STALE_SECS = 300;
    const ageSecs = Date.now() / 1000 - s.updated_at_unix;
    const terminal = s.stage === 'done' || s.stage === 'error';
    if (ageSecs > STALE_SECS && s.stage !== 'awaiting-labels') {
      // A terminal sidecar older than the stale window is not announced.
      if (!terminal) {
        dispatch({ type: 'finalize', sessionId: sid, title: pollSession.title,
          progress: { stage: 'error', progress: s.progress, message: s.message } });
      }
      stopPolling();
      return;
    }
    dispatch({ type: 'finalize', sessionId: sid, title: pollSession.title,
      progress: { stage: s.stage, progress: s.progress, message: s.message } });
    if (terminal) stopPolling();
    // At `done`, read the real session to replace the reducer's placeholders.
    if (s.stage === 'done') void enrichDone(sid);
  };
  pollTimer = setInterval(() => { void tick(); }, 1000);
  void tick();
}

/** Loads the session's summary and speakers and dispatches `enrich-done`.
 *  A fetch failure leaves the hadAi:false placeholder. */
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

/** Starts watching a session's finalize progress on demand. */
export function beginFinalizeWatch(sessionId: string, title: string | null): void {
  startPolling(sessionId, title);
}

function onSnapshotChange(): void {
  dispatch({ type: 'snapshot', snap: getRecordingSnapshot() });
}

function ensureInitialized(): void {
  if (initialized) return;
  initialized = true;
  // recordingState owns the `recording:snapshot` listener, cold fetch and
  // visibility catch-up; change notifications chain through here.
  subscribeRecordingSnapshot(onSnapshotChange);
  subscribeToLibrary((ev) => {
    // library:changed for a watched session polls its sidecar for the terminal stage.
    if (ev.session_id && watched.has(ev.session_id)) {
      startPolling(ev.session_id, pollSession?.title ?? null);
    }
  });
  // Seed from whatever the snapshot store already holds.
  onSnapshotChange();
}

export function dismissPhase(): void { dispatch({ type: 'dismiss' }); }

export function useSessionPhase(): Phase {
  const [, setTick] = useState(0);
  useEffect(() => {
    ensureInitialized();
    const cb = () => setTick((n) => n + 1);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return state.phase;
}

export const __testStore = {
  reset(): void { state = initialState(); subscribers.clear(); stopPolling(); watched.clear(); },
  dispatch(ev: LifecycleEvent): void { dispatch(ev); },
  getPhase(): Phase { return state.phase; },
  subscribe(cb: () => void): () => void { subscribers.add(cb); return () => { subscribers.delete(cb); }; },
};
