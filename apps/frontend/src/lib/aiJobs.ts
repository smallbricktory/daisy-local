// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the backend AI-job registry. The backend owns the work and
// runs one task at a time; this module mirrors the list so any screen can
// gate its Summarize / Extract chapters buttons, and so a running task is
// still visible after navigating away and back.

import { listen } from '@tauri-apps/api/event';
import { useSyncExternalStore } from 'react';
import { tauri, type AiJob, type AiJobKind } from '../tauri';

let jobs: AiJob[] = [];
let started = false;
const subscribers = new Set<() => void>();

function apply(next: AiJob[]): void {
  jobs = next;
  for (const cb of subscribers) cb();
}

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  return () => { subscribers.delete(cb); };
}

/** The AI tasks in flight. */
export function useAiJobs(): AiJob[] {
  return useSyncExternalStore(subscribe, () => jobs);
}

/** Seed from the backend and follow `ai-jobs:changed`. Called once at app
 *  start. A second call re-reads the list and keeps the one subscription. */
export async function initAiJobs(): Promise<void> {
  if (!started) {
    started = true;
    void listen<AiJob[]>('ai-jobs:changed', (e) => { apply(e.payload ?? []); });
  }
  try {
    apply(await tauri.aiJobsList());
  } catch {
    apply([]);
  }
}

/** The running job for this session and kind, or null. */
export function runningJob(list: AiJob[], sessionId: string, kind: AiJobKind): AiJob | null {
  return list.find((j) => j.session_id === sessionId && j.kind === kind) ?? null;
}

/** Returns the module to its pre-subscription state. Tests only. */
export function __resetAiJobsForTests(): void {
  jobs = [];
  started = false;
  subscribers.clear();
}
