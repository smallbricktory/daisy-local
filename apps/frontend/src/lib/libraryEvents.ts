// Single subscription point for backend `library:changed` events. The
// backend emits on every mutation site (added / updated / finalized /
// deleted); the frontend listens and refetches.

import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type LibraryChangeKind = 'added' | 'updated' | 'finalized' | 'deleted';

export interface LibraryChangeEvent {
  kind: LibraryChangeKind;
  session_id: string;
}

let unlisten: UnlistenFn | null = null;
const subscribers = new Set<(ev: LibraryChangeEvent) => void>();

async function ensureListener(): Promise<void> {
  if (unlisten) return;
  unlisten = await listen<LibraryChangeEvent>('library:changed', (ev) => {
    for (const cb of subscribers) cb(ev.payload);
  });
}

/** Subscribe to backend library mutations. The handler is also fired once
 *  on `visibilitychange → visible` (with a synthetic event) to catch up
 *  on events missed while the window was hidden. */
export function subscribeToLibrary(handler: (ev: LibraryChangeEvent) => void): () => void {
  void ensureListener();
  subscribers.add(handler);
  return () => { subscribers.delete(handler); };
}

/** Synthetic refresh signal for the visibility-resume handler below — "treat as
 *  if something might have changed". Fans out to every subscriber. */
function notifyLibraryStale(): void {
  const ev: LibraryChangeEvent = { kind: 'updated', session_id: '' };
  for (const cb of subscribers) cb(ev);
}

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') notifyLibraryStale();
  });
}
