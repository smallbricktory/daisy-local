import { describe, it, expect, vi, beforeEach } from 'vitest';

// Controls when tauri.qaAskStream resolves (interleaving cancel/supersede)
// and captures its onToken callback to simulate streamed deltas.
let resolveAsk: (v: unknown) => void;
let rejectAsk: (e: unknown) => void;
let lastOnToken: (delta: string) => void;
const qaAskStream = vi.fn((_q: string, onToken: (d: string) => void) => {
  lastOnToken = onToken;
  return new Promise((res, rej) => { resolveAsk = res; rejectAsk = rej; });
});
vi.mock('../tauri', () => ({ tauri: { qaAskStream: (q: string, cb: (d: string) => void) => qaAskStream(q, cb) } }));

import { askQuestion, cancelQuestion, getQaState, __qaTestStore } from './qaStore';

const answer = { answer: 'hi', citations: [], indexed_sessions: 1, total_chunks: 2 };

beforeEach(() => { __qaTestStore.reset(); qaAskStream.mockClear(); });

describe('qaStore', () => {
  it('ask → asking → streams partial → done', async () => {
    const p = askQuestion('what?');
    expect(getQaState().status).toBe('asking');
    expect(getQaState().query).toBe('what?');
    // Streamed deltas accumulate into `partial` while asking.
    lastOnToken('he');
    lastOnToken('llo');
    expect(getQaState().partial).toBe('hello');
    resolveAsk(answer);
    await p;
    expect(getQaState().status).toBe('done');
    expect(getQaState().answer).toEqual(answer);
    expect(getQaState().partial).toBe('');
  });

  it('cancel discards a late-resolving result', async () => {
    const p = askQuestion('slow?');
    expect(getQaState().status).toBe('asking');
    cancelQuestion();
    expect(getQaState().status).toBe('cancelled');
    resolveAsk(answer); // arrives after cancel — must be ignored
    await p;
    expect(getQaState().status).toBe('cancelled');
    expect(getQaState().answer).toBeNull();
  });

  it('surfaces an error', async () => {
    const p = askQuestion('boom?');
    rejectAsk(new Error('HTTP 400'));
    await p;
    expect(getQaState().status).toBe('error');
    expect(getQaState().error).toContain('HTTP 400');
  });

  it('guards against double-submit while one ask is in flight', async () => {
    const p = askQuestion('first?');
    await askQuestion('second?'); // ignored — already asking
    expect(qaAskStream).toHaveBeenCalledTimes(1);
    expect(getQaState().query).toBe('first?');
    resolveAsk(answer);
    await p;
  });
});
