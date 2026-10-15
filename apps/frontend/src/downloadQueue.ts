// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the Rust download queue. The queue owns the work; this
// module mirrors it: it seeds from `download_queue_status` on the first
// subscription and follows `download-queue:changed` after that. Settings →
// Providers, the wizard and the main-window toast all read the same list.
// How a job ended arrives on its own, as `download:finished`.

import { backendStore } from './lib/backendStore';
import { useBackendEvent } from './lib/useBackendEvent';
import { tauri, type DownloadFinished, type DownloadJob, type DownloadKind } from './tauri';

const queue = backendStore<DownloadJob[]>(
  'download-queue:changed', () => tauri.downloadQueueStatus(), [],
);

export function useDownloadQueue(): DownloadJob[] {
  return queue.use();
}

export function useDownloadJob(id: string): DownloadJob | undefined {
  return useDownloadQueue().find((j) => j.id === id);
}

/** Runs `handler` with each download that ends: installed, cancelled, or
 *  failed with its attempts spent. */
export function useDownloadFinished(handler: (f: DownloadFinished) => void): void {
  useBackendEvent<DownloadFinished>('download:finished', handler);
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
  if (/sha256 mismatch/.test(e)) {
    return {
      cause: 'The downloaded file did not match the checksum Daisy has for it',
      advice: 'Daisy discarded it. Try again on a different network.',
    };
  }
  if (/size mismatch/.test(e)) {
    return {
      cause: 'The download server sent a larger file than Daisy expects',
      advice: 'Daisy discarded it. Try again on a different network.',
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
  await tauri.downloadQueueCancel(id);
}

export async function retryDownload(id: string): Promise<void> {
  await tauri.downloadQueueRetry(id);
}

/** Returns the module to its pre-subscription state. Tests only. */
export const __resetDownloadQueueForTests = queue.reset;
