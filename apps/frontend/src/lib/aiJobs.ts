// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the backend AI-job registry. The backend owns the work and
// runs one task at a time; this module mirrors the list (`ai_jobs_list`, then
// `ai-jobs:changed`) so any screen can gate its Summarize / Extract chapters
// buttons, and so a running task is still visible after navigating away and
// back.

import { backendStore } from './backendStore';
import { tauri, type AiJob, type AiJobKind } from '../tauri';

const store = backendStore<AiJob[]>('ai-jobs:changed', () => tauri.aiJobsList(), []);

/** The AI tasks in flight. */
export function useAiJobs(): AiJob[] {
  return store.use();
}

/** The running job for this session and kind, or null. */
export function runningJob(list: AiJob[], sessionId: string, kind: AiJobKind): AiJob | null {
  return list.find((j) => j.session_id === sessionId && j.kind === kind) ?? null;
}

/** Returns the module to its pre-subscription state. Tests only. */
export const __resetAiJobsForTests = store.reset;
