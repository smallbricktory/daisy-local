// Shared tag dictionary: one module-level cache over `list_tags`, shared by
// every consumer. Stays fresh two ways:
//   * `library:changed` bus — the backend emits on every session mutation
//     (tag assignment lands here as `updated`, which moves `use_count`), and
//     the bus adds a synthetic event on visibility-resume.
//   * `invalidateTags()` — called by the frontend after tag CRUD
//     (create/update/delete have no backend emit).

import { useEffect, useState } from 'react';
import { tauri, type Tag } from '../tauri';
import { subscribeToLibrary } from './libraryEvents';

let cache: Tag[] | null = null;
let busAttached = false;
const listeners = new Set<(t: Tag[]) => void>();

async function fetchTags(): Promise<void> {
  try {
    const t = await tauri.listTags();
    cache = t;
    for (const l of listeners) l(t);
  } catch {
    /* vault locked / backend not ready — keep the prior list */
  }
}

/** Refetch the shared tag list. Call after any tag mutation; awaitable when
 *  the caller needs the cache updated before proceeding (e.g. picking a tag
 *  that was just created). */
export function invalidateTags(): Promise<void> {
  return fetchTags();
}

function ensureBus(): void {
  if (busAttached) return;
  busAttached = true;
  subscribeToLibrary(() => void fetchTags());
}

/** Subscribe to the shared tag list. Fetches once on first mount across the
 *  app; every subscriber sees the same array. Empty until the first fetch
 *  resolves. */
export function useTags(): Tag[] {
  const [tags, setTags] = useState<Tag[]>(cache ?? []);
  useEffect(() => {
    ensureBus();
    listeners.add(setTags);
    if (cache == null) void fetchTags();
    else setTags(cache);
    return () => { listeners.delete(setTags); };
  }, []);
  return tags;
}
