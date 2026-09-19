// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Singleton store for the active recording snapshot: one listener against
// the backend's `recording:snapshot` event, shared by every consumer.
//
// Backend emits:
//   * `recording:snapshot` with the full RecordingSnapshot on start /
//     pause / resume, and `null` on stop / cancel / delete-while-recording.
//
// A one-shot cold fetch on first mount + a refresh on
// `visibilitychange → visible` covers: app launched mid-recording (no event
// yet), events missed while the window was hidden.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { useEffect, useState } from 'react';
import { tauri, type RecordingSnapshot } from '../tauri';

type Snap = RecordingSnapshot | null;

let snapshot: Snap = null;
let initialized = false;
let unlisten: UnlistenFn | null = null;
const subscribers = new Set<() => void>();

function notify(): void {
  for (const cb of subscribers) cb();
}

function patch(next: Snap): void {
  // Identical-payload events do not notify.
  if (next === snapshot) return;
  if (next && snapshot
    && next.state === snapshot.state
    && next.session_id === snapshot.session_id
    && next.started_at_unix_seconds === snapshot.started_at_unix_seconds
  ) return;
  snapshot = next;
  notify();
}

async function attachListener(): Promise<void> {
  if (unlisten) return;
  // Backend emits the full snapshot (or null) here. State strings still
  // land on `recording:state` but those are redundant once you have the
  // snapshot — keep just one channel.
  unlisten = await listen<Snap>('recording:snapshot', (ev) => patch(ev.payload));
}

async function coldFetch(): Promise<void> {
  try {
    const s = await tauri.recordingSnapshot();
    patch(s);
  } catch {
    /* fine — backend may not be ready yet on first mount */
  }
}

async function ensureInitialized(): Promise<void> {
  if (initialized) return;
  initialized = true;
  await attachListener();
  await coldFetch();
}

// Visibility-resume catch-up: if events fired while hidden we may have
// missed them. Refresh on every visible-transition.
if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') void coldFetch();
  });
}

/** Current snapshot (null when not recording). */
export function getRecordingSnapshot(): Snap {
  return snapshot;
}

/** Module-level subscription for non-React consumers (chained stores).
 *  Triggers the one-shot init; read via getRecordingSnapshot(). */
export function subscribeRecordingSnapshot(cb: () => void): () => void {
  void ensureInitialized();
  subscribers.add(cb);
  return () => { subscribers.delete(cb); };
}

/** React hook: subscribe to recording-state changes. Returns the current
 *  snapshot (null when not recording). Triggers the one-shot init on
 *  first mount across the app. */
export function useRecordingState(): Snap {
  const [, setTick] = useState(0);
  useEffect(() => subscribeRecordingSnapshot(() => setTick((n) => n + 1)), []);
  return snapshot;
}
