// Singleton store for the active recording snapshot: one listener against
// the backend's `recording:snapshot` event, shared by every consumer (App
// rail label, RecordingIndicator pill, MiniWindow).
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

/** React hook: subscribe to recording-state changes. Returns the current
 *  snapshot (null when not recording). Triggers the one-shot init on
 *  first mount across the app. */
export function useRecordingState(): Snap {
  const [, setTick] = useState(0);
  useEffect(() => {
    void ensureInitialized();
    const cb = () => setTick((n) => n + 1);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return snapshot;
}
