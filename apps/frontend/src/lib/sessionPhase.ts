// Pure session-lifecycle phase model + event→phase reducer. No React, no
// Tauri runtime — the single source for the session-status toast's phases.
import type { RecordingSnapshot, FinalizeProgress } from '../tauri';

export type FinalizeStage = FinalizeProgress['stage'];

/** Which backend pipeline a manual Summarize/Regen kicks off. */
export type JobKind = 'cascade' | 'regen-summary' | 'regen-transcript' | 'polish-transcript';

// Canonical forward pipeline order, used for the "step N of M" toast.
// 'resuming' (resume-path entry) and 'error' are valid stages outside the
// linear forward count and are omitted. 'polishing' is also omitted: polish
// is an on-demand action and never appears in finalize progress; the type
// still includes it for any in-flight sidecar, and indexOf(-1) falls back to
// step 1.
export const FINALIZE_STAGES: FinalizeStage[] = [
  'finalizing', 'echo-cancelling', 'transcribing', 'deduping',
  'awaiting-labels', 'summarizing', 'chaptering', 'compressing', 'done',
];

export interface DoneSummary { hadAi: boolean; speakers: number | null; durationLabel: string | null; }

export type Phase =
  | { kind: 'idle' }
  | { kind: 'recording'; snap: RecordingSnapshot }
  | { kind: 'finalizing'; sessionId: string; title: string | null; stage: FinalizeStage; progress: number }
  | { kind: 'needs-labels'; sessionId: string; title: string | null; clusters: number[] }
  | { kind: 'done'; sessionId: string; title: string | null; summary: DoneSummary }
  | { kind: 'interrupted'; sessionId: string; title: string | null; lastStage: FinalizeStage }
  | { kind: 'hidden' };

export type PhaseKind = Phase['kind'];

export function initialPhase(): Phase { return { kind: 'idle' }; }

export function dismissKey(sessionId: string, kind: PhaseKind): string { return `${sessionId}:${kind}`; }

export interface LifecycleState { phase: Phase; dismissed: Set<string>; }

export type LifecycleEvent =
  | { type: 'snapshot'; snap: RecordingSnapshot | null }
  | { type: 'finalize'; sessionId: string; title: string | null; progress: FinalizeProgress }
  | { type: 'enrich-done'; sessionId: string; summary: DoneSummary }
  | { type: 'dismiss' };

export function initialState(): LifecycleState { return { phase: initialPhase(), dismissed: new Set() }; }

function parseClusters(message: string | null): number[] {
  if (!message) return [];
  try { const v = JSON.parse(message); return Array.isArray(v) ? v.filter((n) => typeof n === 'number') : []; }
  catch { return []; }
}

function finalizePhase(sessionId: string, title: string | null, p: FinalizeProgress): Phase {
  if (p.stage === 'error') return { kind: 'interrupted', sessionId, title, lastStage: 'finalizing' };
  if (p.stage === 'awaiting-labels') return { kind: 'needs-labels', sessionId, title, clusters: parseClusters(p.message) };
  if (p.stage === 'done') return { kind: 'done', sessionId, title, summary: { hadAi: false, speakers: null, durationLabel: null } };
  return { kind: 'finalizing', sessionId, title, stage: p.stage, progress: p.progress };
}

export function reducer(state: LifecycleState, ev: LifecycleEvent): LifecycleState {
  switch (ev.type) {
    case 'dismiss': {
      if (state.phase.kind === 'idle' || state.phase.kind === 'hidden') return state;
      const sid = 'sessionId' in state.phase ? state.phase.sessionId
        : 'snap' in state.phase ? state.phase.snap.session_id : null;
      const dismissed = new Set(state.dismissed);
      if (sid) dismissed.add(dismissKey(sid, state.phase.kind));
      return { phase: { kind: 'hidden' }, dismissed };
    }
    case 'snapshot': {
      if (ev.snap == null) {
        if (state.phase.kind === 'recording') return { ...state, phase: { kind: 'idle' } };
        return state;
      }
      return { ...state, phase: { kind: 'recording', snap: ev.snap } };
    }
    case 'finalize': {
      let target = finalizePhase(ev.sessionId, ev.title, ev.progress);
      if (ev.progress.stage === 'error' && state.phase.kind === 'finalizing'
          && state.phase.sessionId === ev.sessionId) {
        target = { ...(target as Extract<Phase, { kind: 'interrupted' }>), lastStage: state.phase.stage };
      }
      if (state.dismissed.has(dismissKey(ev.sessionId, target.kind))) {
        return { ...state, phase: { kind: 'hidden' } };
      }
      return { ...state, phase: target };
    }
    case 'enrich-done': {
      if (state.phase.kind === 'done' && state.phase.sessionId === ev.sessionId) {
        return { ...state, phase: { ...state.phase, summary: ev.summary } };
      }
      return state;
    }
    default:
      return state;
  }
}
