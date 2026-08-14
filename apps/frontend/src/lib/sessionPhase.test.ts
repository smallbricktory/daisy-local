import { describe, it, expect } from 'vitest';
import {
  dismissKey, initialPhase, initialState, reducer,
  type LifecycleState,
} from './sessionPhase';

describe('dismissKey', () => {
  it('combines session id and phase kind', () => {
    expect(dismissKey('s1', 'done')).toBe('s1:done');
    expect(dismissKey('s1', 'needs-labels')).toBe('s1:needs-labels');
  });
});

describe('initialPhase', () => {
  it('starts idle', () => { expect(initialPhase().kind).toBe('idle'); });
});

const S0: LifecycleState = initialState();
const snap = (state: 'recording' | 'paused' | 'stopped', id = 's1') => ({
  state, session_id: id, session_root: '/x', started_at_unix_seconds: 1000, live_mode_label: 'off',
});

describe('reducer', () => {
  it('snapshot(recording) → recording', () => {
    expect(reducer(S0, { type: 'snapshot', snap: snap('recording') }).phase.kind).toBe('recording');
  });
  it('snapshot(null) from recording → idle', () => {
    const rec = reducer(S0, { type: 'snapshot', snap: snap('recording') });
    expect(reducer(rec, { type: 'snapshot', snap: null }).phase.kind).toBe('idle');
  });
  it('finalize → finalizing with stage + title', () => {
    const s = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'transcribing', progress: 0.3, message: null } });
    expect(s.phase).toMatchObject({ kind: 'finalizing', sessionId: 's1', title: 'Q3', stage: 'transcribing' });
  });
  it('awaiting-labels → needs-labels with parsed clusters', () => {
    const s = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'awaiting-labels', progress: 0.5, message: '[0,2]' } });
    expect(s.phase).toMatchObject({ kind: 'needs-labels', sessionId: 's1', clusters: [0, 2] });
  });
  it('done → done', () => {
    const s = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    expect(s.phase.kind).toBe('done');
  });
  it('enrich-done replaces the done summary for the matching session', () => {
    const done = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    expect(done.phase).toMatchObject({ kind: 'done', summary: { hadAi: false } });
    const enriched = reducer(done, { type: 'enrich-done', sessionId: 's1', summary: { hadAi: true, speakers: 3, durationLabel: null } });
    expect(enriched.phase).toMatchObject({ kind: 'done', summary: { hadAi: true, speakers: 3 } });
  });
  it('enrich-done for a different session id is ignored', () => {
    const done = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    const enriched = reducer(done, { type: 'enrich-done', sessionId: 's2', summary: { hadAi: true, speakers: 3, durationLabel: null } });
    expect(enriched).toBe(done);
    expect(enriched.phase).toMatchObject({ kind: 'done', summary: { hadAi: false } });
  });
  it('error → interrupted carrying last finalizing stage', () => {
    const fin = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'summarizing', progress: 0.8, message: null } });
    const s = reducer(fin, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'error', progress: 0.8, message: 'boom' } });
    expect(s.phase).toMatchObject({ kind: 'interrupted', lastStage: 'summarizing' });
  });
  it('dismiss hides; a later same-phase event does NOT reopen (reopen-bug guard)', () => {
    const done = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    const dismissed = reducer(done, { type: 'dismiss' });
    expect(dismissed.phase.kind).toBe('hidden');
    const again = reducer(dismissed, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    expect(again.phase.kind).toBe('hidden');
  });
  it('a NEW session after dismiss is not suppressed', () => {
    const done = reducer(S0, { type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    const dismissed = reducer(done, { type: 'dismiss' });
    const s2 = reducer(dismissed, { type: 'snapshot', snap: snap('recording', 's2') });
    expect(s2.phase.kind).toBe('recording');
  });
});
