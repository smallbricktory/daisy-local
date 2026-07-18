import { describe, it, expect, beforeEach } from 'vitest';
import { __testStore } from './sessionLifecycle';

describe('lifecycle store', () => {
  beforeEach(() => __testStore.reset());

  it('dispatch updates phase and notifies subscribers', () => {
    let ticks = 0;
    const off = __testStore.subscribe(() => { ticks += 1; });
    __testStore.dispatch({ type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'transcribing', progress: 0.3, message: null } });
    expect(__testStore.getPhase().kind).toBe('finalizing');
    expect(ticks).toBeGreaterThan(0);
    off();
  });

  it('dismiss hides and stays hidden on a repeat event', () => {
    __testStore.dispatch({ type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    __testStore.dispatch({ type: 'dismiss' });
    expect(__testStore.getPhase().kind).toBe('hidden');
    __testStore.dispatch({ type: 'finalize', sessionId: 's1', title: 'Q3', progress: { stage: 'done', progress: 1, message: null } });
    expect(__testStore.getPhase().kind).toBe('hidden');
  });
});
