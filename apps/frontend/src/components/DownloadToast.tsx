// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Headless driver projecting the download queue onto the toast stack. A
// download that the wizard or Settings → Providers started keeps a presence
// in the main window wherever the user goes: one working toast per family
// while it runs, a short one when it lands, and the error with a Retry when
// it does not.
import { useEffect, useRef } from 'react';
import { explainDownloadError, retryDownload, useDownloadQueue, wasCancelled } from '../downloadQueue';
import { pushToast, dismissToast } from '../lib/toastStore';
import { revalidateAiProviderStatus } from '../lib/aiProviderStatus';
import type { DownloadJob, DownloadKind } from '../tauri';

const FAMILIES: Array<{ id: string; kinds: DownloadKind[]; title: string; ready: string }> = [
  {
    id: 'download-llm',
    kinds: ['llm'],
    title: "Downloading Daisy's AI model",
    ready: "Daisy's AI model is ready",
  },
  {
    id: 'download-asr',
    kinds: ['asr', 'sa-assets'],
    title: 'Downloading speech model',
    ready: 'Speech model is ready',
  },
];

function bodyOf(job: DownloadJob): string {
  switch (job.phase.state) {
    case 'queued': return 'Waiting to start';
    case 'downloading': return job.indeterminate ? 'Downloading…' : `${Math.round(job.phase.pct * 100)}%`;
    case 'warming': return 'Getting it ready…';
    case 'retrying': {
      const { attempt, of } = job.phase;
      return `Trying again — attempt ${attempt} of ${of}. ${explainDownloadError(job.phase.error).cause}.`;
    }
    case 'failed': {
      const { cause, advice } = explainDownloadError(job.phase.error);
      return `${cause}. ${advice}`;
    }
  }
}

export function DownloadToast(): null {
  const jobs = useDownloadQueue();
  const previous = useRef<DownloadJob[]>([]);
  useEffect(() => {
    for (const family of FAMILIES) {
      const job = jobs.find((j) => family.kinds.includes(j.kind));
      const before = previous.current.find((j) => family.kinds.includes(j.kind));
      // The built-in model's AI-provider status turns on this job's phase;
      // every category change refreshes the shared cache the gates read.
      if (family.id === 'download-llm' && job?.phase.state !== before?.phase.state) {
        revalidateAiProviderStatus();
      }
      const announceReady = () => pushToast({
        id: family.id,
        severity: 'done',
        title: family.ready,
        autoDismissMs: 6000,
      });
      if (job && job.phase.state === 'failed') {
        if (before?.phase.state !== 'failed') {
          console.warn('download failed:', job.id, job.phase.error);
        }
        pushToast({
          id: family.id,
          severity: 'error',
          title: family.title,
          body: bodyOf(job),
          actions: [{ label: 'Retry', onClick: () => { void retryDownload(job.id); } }],
        });
      } else if (job) {
        pushToast({
          id: family.id,
          severity: 'working',
          title: family.title,
          body: bodyOf(job),
          progress:
            job.phase.state === 'downloading' && !job.indeterminate ? job.phase.pct : undefined,
        });
      } else if (before && before.phase.state === 'warming') {
        // The shards land and the provider default is set even when the
        // warm-up was cancelled mid-flight — a cancel there does not stop
        // the install, so it is still a completion.
        announceReady();
      } else if (before && wasCancelled(before.id)) {
        // A cancel before warming is not a completion — dismiss without
        // announcing "ready".
        dismissToast(family.id);
      } else if (before && before.phase.state !== 'failed') {
        announceReady();
      } else if (before) {
        dismissToast(family.id);
      }
    }
    previous.current = jobs;
  }, [jobs]);
  return null;
}
