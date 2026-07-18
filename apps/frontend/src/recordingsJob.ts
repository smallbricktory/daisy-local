// Module-level store for the long-running "clear all" job in Settings →
// Recordings. The job is kicked off by the section component but runs
// detached from it; progress survives navigating away and back (the
// component re-subscribes on mount and reads the current snapshot).

import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { tauri, formatBytes } from './tauri';

export interface RecProgress {
  done: number;
  total: number;
  current: string;
}

export type RecJobKind = 'delete';

export interface RecJobState {
  /** null = idle. */
  kind: RecJobKind | null;
  progress: RecProgress | null;
  /** Result/error text from the last completed job; cleared when a new one starts. */
  message: string | null;
}

let state: RecJobState = { kind: null, progress: null, message: null };
const subscribers = new Set<() => void>();

function emit(): void {
  for (const cb of subscribers) cb();
}

function patch(next: Partial<RecJobState>): void {
  state = { ...state, ...next };
  emit();
}

export function subscribeRecJob(cb: () => void): () => void {
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

export function getRecJobState(): RecJobState {
  return state;
}

async function run(kind: RecJobKind, task: () => Promise<string>): Promise<void> {
  if (state.kind !== null) return; // a job is already running
  patch({ kind, progress: { done: 0, total: 0, current: '' }, message: null });
  let un: UnlistenFn | undefined;
  un = await listen<RecProgress>('daisy://recordings/delete_progress', (e) => patch({ progress: e.payload }));
  try {
    const msg = await task();
    patch({ message: msg });
  } catch (e: unknown) {
    patch({ message: String(e) });
  } finally {
    if (un) un();
    patch({ kind: null, progress: null });
  }
}

export function startDeleteAll(): Promise<void> {
  return run('delete', async () => {
    const s = await tauri.recordingsDeleteAll();
    return `Cleared audio from ${s.deleted_sessions} session${s.deleted_sessions === 1 ? '' : 's'} — freed ${formatBytes(
      s.freed_bytes,
    )}. Transcripts, summaries and notes were kept.`;
  });
}
