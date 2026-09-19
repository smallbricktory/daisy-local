// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the Rust download queue. The queue owns the work; this
// module mirrors it: it seeds from `download_queue_status` on the first
// subscription and follows `download-queue:changed` after that. Settings →
// Providers, the wizard and the main-window toast all read the same list.

import { listen } from '@tauri-apps/api/event';
import { useSyncExternalStore } from 'react';
import { tauri, type DownloadJob, type DownloadKind } from './tauri';

let jobs: DownloadJob[] = [];
let finished = 0;
let started = false;
let seenEvent = false;
const subscribers = new Set<() => void>();
/** Ids `cancelDownload` was called for, so a departing job can be told a
 *  cancel from a completion. Clears when an id that was absent from the
 *  previous snapshot appears in a new one — a fresh enqueue or retry starts
 *  the id over. A snapshot that merely still carries the id (a progress
 *  emit landing between the cancel mark and the backend's removal) leaves
 *  the mark in place. */
const cancelledIds = new Set<string>();

function notify(): void {
  for (const cb of subscribers) cb();
}

/** A job that leaves the list without a failed phase has done its work — a
 *  finished download or a cancel. Both change what the model rows say. */
function apply(next: DownloadJob[]): void {
  const previousIds = new Set(jobs.map((j) => j.id));
  const gone = jobs.filter(
    (j) => j.phase.state !== 'failed' && !next.some((n) => n.id === j.id),
  );
  jobs = next;
  finished += gone.length;
  for (const j of next) {
    if (!previousIds.has(j.id)) cancelledIds.delete(j.id);
  }
  notify();
}

function start(): void {
  if (started) return;
  started = true;
  void listen<DownloadJob[]>('download-queue:changed', (e) => { seenEvent = true; apply(e.payload); });
  void tauri.downloadQueueStatus().then((jobs) => { if (!seenEvent) apply(jobs); }).catch(() => {});
}

function subscribe(cb: () => void): () => void {
  start();
  subscribers.add(cb);
  return () => { subscribers.delete(cb); };
}

export function useDownloadQueue(): DownloadJob[] {
  return useSyncExternalStore(subscribe, () => jobs);
}

export function useDownloadJob(id: string): DownloadJob | undefined {
  return useDownloadQueue().find((j) => j.id === id);
}

/** Bumped each time a job leaves the list; mounted views re-read their rows. */
export function useDownloadsFinished(): number {
  return useSyncExternalStore(subscribe, () => finished);
}

/** What a download failure is told to the user: the cause in plain words and
 *  what they can do about it. The underlying error text goes to the log. */
export function explainDownloadError(error: string): { cause: string; advice: string } {
  const e = error.toLowerCase();
  if (/no space|not enough space|disk full|quota/.test(e)) {
    return {
      cause: 'There is not enough free space for this model',
      advice: 'Free up some space, then try again.',
    };
  }
  if (/permission denied|access is denied|read-only|not permitted/.test(e)) {
    return {
      cause: 'Daisy could not write to its models folder',
      advice: "Check the folder's permissions, then try again.",
    };
  }
  if (/worker missing/.test(e)) {
    return {
      cause: "Daisy's model engine is missing from this install",
      advice: 'Reinstall Daisy.',
    };
  }
  if (/warm-?up/.test(e)) {
    return {
      cause: 'The model downloaded, but would not start on this computer',
      advice: 'Try again.',
    };
  }
  if (/sha256 mismatch/.test(e)) {
    return {
      cause: 'The downloaded file did not match the checksum Daisy has for it',
      advice: 'Daisy discarded it. Try again on a different network.',
    };
  }
  if (/is missing|unpack|corrupt/.test(e)) {
    return {
      cause: 'The downloaded file arrived damaged',
      advice: 'Try the download again.',
    };
  }
  if (/timed out|timeout|connection|no route|unreachable|failed to lookup address|\bdns\b|error sending request|error status|\b50[234]\b/.test(e)) {
    return {
      cause: 'Daisy could not reach the download server',
      advice: 'Check your internet connection, then try again.',
    };
  }
  return { cause: 'The download did not finish', advice: 'Try again.' };
}

/** What a model row, the wizard and the toast say beside a download that is
 *  going wrong: the cause in plain words, and the attempt that failed while
 *  the entry is still waiting to try again. */
export function downloadMessage(job: DownloadJob | undefined): string | null {
  if (job === undefined) return null;
  if (job.phase.state === 'failed') {
    const { cause, advice } = explainDownloadError(job.phase.error);
    return `${cause}. ${advice}`;
  }
  if (job.phase.state === 'retrying') {
    return `Attempt ${job.phase.attempt - 1} failed: ${explainDownloadError(job.phase.error).cause}.`;
  }
  return null;
}

export async function enqueueDownload(kind: DownloadKind, id: string): Promise<void> {
  await tauri.downloadQueueEnqueue(kind, id);
}

export async function cancelDownload(id: string): Promise<void> {
  cancelledIds.add(id);
  await tauri.downloadQueueCancel(id);
}

/** Whether `cancelDownload` was called for this id and no later snapshot has
 *  shown a job with the same id since. */
export function wasCancelled(id: string): boolean {
  return cancelledIds.has(id);
}

export async function retryDownload(id: string): Promise<void> {
  await tauri.downloadQueueRetry(id);
}

/** Returns the module to its pre-subscription state. Tests only. */
export function __resetDownloadQueueForTests(): void {
  jobs = [];
  finished = 0;
  started = false;
  seenEvent = false;
  subscribers.clear();
  cancelledIds.clear();
}
