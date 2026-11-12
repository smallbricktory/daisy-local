// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Singleton store for the active recording snapshot: one listener against
// the backend's `recording:snapshot` event, shared by every consumer.
//
// Backend emits:
//   * `recording:snapshot` with the full RecordingSnapshot on start /
//     pause / resume, and `null` on stop / cancel / delete-while-recording.
//
// The store's cold fetch covers an app launched mid-recording (no event
// yet); its visibility re-read covers events missed while hidden.

import { useSyncExternalStore } from 'react';
import { backendStore } from './backendStore';
import { tauri, type RecordingSnapshot } from '../tauri';

type Snap = RecordingSnapshot | null;

/** Identical-payload events do not notify. */
function sameSnap(a: Snap, b: Snap): boolean {
  if (a === b) return true;
  return a !== null && b !== null
    && a.state === b.state
    && a.session_id === b.session_id
    && a.started_at_unix_seconds === b.started_at_unix_seconds
    && a.recorded_ms === b.recorded_ms
    && a.clock_running_since_unix_ms === b.clock_running_since_unix_ms
    && a.live_mode_label === b.live_mode_label;
}

/** The recording clock in whole seconds: the audio recorded, paused spans
 *  and capture gaps excluded. Runs on from the snapshot's reading while
 *  recording; holds still while paused or while capture has stalled. */
export function recordedSeconds(snap: RecordingSnapshot, nowMs: number): number {
  const since = snap.clock_running_since_unix_ms;
  const running = since === null ? 0 : Math.max(0, nowMs - since);
  return Math.floor((snap.recorded_ms + running) / 1000);
}

const store = backendStore<Snap>('recording:snapshot', () => tauri.recordingSnapshot(), null, sameSnap);

// A session this window handed to the finalize cascade reads as not
// recording until the backend stops reporting it.
let released: string | null = null;
let watchingRelease = false;
const releaseListeners = new Set<() => void>();

function current(): Snap {
  const snap = store.get();
  return snap && snap.session_id === released ? null : snap;
}

function subscribe(cb: () => void): () => void {
  const off = store.subscribe(cb);
  releaseListeners.add(cb);
  return () => { off(); releaseListeners.delete(cb); };
}

/** Current snapshot (null when not recording). */
export const getRecordingSnapshot = current;

/** Reads the snapshot again. */
export const refreshRecordingSnapshot = store.refresh;

/** Module-level subscription for non-React consumers (chained stores).
 *  Read via getRecordingSnapshot(). */
export const subscribeRecordingSnapshot = subscribe;

/** React hook: the current snapshot (null when not recording). */
export function useRecordingState(): Snap {
  return useSyncExternalStore(subscribe, current);
}

/** Marks the recording `sessionId` as finished by this window: it reads as
 *  not recording until the backend reports the stop. */
export function releaseRecording(sessionId: string): void {
  if (store.get()?.session_id !== sessionId) return;
  released = sessionId;
  if (!watchingRelease) {
    watchingRelease = true;
    store.subscribe(() => {
      if (store.get()?.session_id !== released) released = null;
    });
  }
  for (const cb of releaseListeners) cb();
}
