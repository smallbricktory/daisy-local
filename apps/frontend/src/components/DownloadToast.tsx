// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Headless driver projecting the download queue onto the toast stack: one
// working toast per family while a download runs, a short one when it lands
// (`download:finished`), and the error with a Retry when it fails.
import { useEffect, useRef } from 'react';
import { explainDownloadError, retryDownload, useDownloadFinished, useDownloadQueue } from '../downloadQueue';
import { useBuiltinModel } from '../builtinModel';
import { pushToast, dismissToast } from '../lib/toastStore';
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

const LLM = FAMILIES[0];

function announceReady(family: (typeof FAMILIES)[number]): void {
  pushToast({ id: family.id, severity: 'done', title: family.ready, autoDismissMs: 6000 });
}

export function DownloadToast(): null {
  const jobs = useDownloadQueue();
  const builtin = useBuiltinModel()?.model;
  const warming = builtin?.warming ?? false;
  const installed = builtin?.installed ?? false;
  /** The built-in model downloaded or is warming, and its toast waits for
   *  the warm-up to end. */
  const llmPending = useRef(false);

  // A job in the queue: its progress, or its error with a Retry.
  useEffect(() => {
    for (const family of FAMILIES) {
      const job = jobs.find((j) => family.kinds.includes(j.kind));
      if (!job) continue;
      pushToast(job.phase.state === 'failed'
        ? {
          id: family.id,
          severity: 'error',
          title: family.title,
          body: bodyOf(job),
          actions: [{ label: 'Retry', onClick: () => { void retryDownload(job.id); } }],
        }
        : {
          id: family.id,
          severity: 'working',
          title: family.title,
          body: bodyOf(job),
          progress:
            job.phase.state === 'downloading' && !job.indeterminate ? job.phase.pct : undefined,
        });
    }
  }, [jobs]);

  useDownloadFinished((f) => {
    const family = FAMILIES.find((fam) => fam.kinds.includes(f.kind));
    if (!family) return;
    if (f.outcome === 'failed') {
      const job = jobs.find((j) => j.id === f.id);
      console.warn('download failed:', f.id, job?.phase.state === 'failed' ? job.phase.error : '');
      return;
    }
    if (f.outcome === 'cancelled') {
      if (family === LLM) llmPending.current = false;
      dismissToast(family.id);
      return;
    }
    if (family !== LLM) { announceReady(family); return; }
    // The built-in model's ready toast waits for its warm-up to end.
    if (installed && !warming) announceReady(LLM);
    else llmPending.current = true;
  });

  // The built-in model warming up — after its download, or when it is picked
  // again — keeps the toast working; ready follows, and a delete dismisses.
  useEffect(() => {
    if (warming) {
      llmPending.current = true;
      pushToast({ id: LLM.id, severity: 'working', title: LLM.title, body: 'Getting it ready…' });
    } else if (llmPending.current && builtin) {
      llmPending.current = false;
      if (installed) announceReady(LLM);
      else dismissToast(LLM.id);
    }
  }, [warming, installed, builtin]);
  return null;
}
