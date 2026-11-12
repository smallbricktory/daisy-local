// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Whether a content key of the profile is open, as `encryption_status` last
// answered. The status is asked again each time the backend announces
// `encryption:keys_changed`: a key opened (the recovery words, a new key, or
// another desktop's key that synced in) or a vault file that synced in was
// tried.
import { useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';
import { tauri, type EncryptionStatus } from '../tauri';

export type ContentKeyState = EncryptionStatus['content_key'];

let current: ContentKeyState = 'ok';
const listeners = new Set<() => void>();
let following = false;

function set(next: ContentKeyState): void {
  if (next === current) return;
  current = next;
  for (const l of listeners) l();
}

/** Records a status the caller already asked for. */
export function noteContentKey(state: ContentKeyState): void {
  set(state);
  follow();
}

/** Asks `encryption_status` again; a failed ask leaves the state as it was. */
export async function refreshContentKey(): Promise<ContentKeyState> {
  const s = await tauri.encryptionStatus().catch(() => null);
  if (s) noteContentKey(s.content_key);
  return current;
}

/** Listens, once, for the backend's announcements of a change. */
function follow(): void {
  if (following) return;
  following = true;
  void listen('encryption:keys_changed', () => { void refreshContentKey(); }).catch(() => { following = false; });
}

export function getContentKey(): ContentKeyState {
  return current;
}

function subscribe(l: () => void): () => void {
  listeners.add(l);
  return () => { listeners.delete(l); };
}

export function useContentKey(): ContentKeyState {
  return useSyncExternalStore(subscribe, getContentKey);
}
