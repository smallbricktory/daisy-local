// Singleton store for the Search "Ask AI" flow. The ask (which can take many
// seconds) survives navigation: back on Search, the in-flight/finished
// answer is still there, and a toast offers a way back.
//
// Cancellation is frontend-only: the Tauri `qa_ask` command runs to
// completion in a spawn_blocking task (no backend cancel token). Cancel here
// means "stop waiting and discard the result" — enforced by a generation
// counter that invalidates any resolve from a superseded/cancelled ask.
import { useEffect, useState } from 'react';
import { tauri, type QaAnswer } from '../tauri';
import { showGatewayNoticeIfNeeded } from './gatewayNotice';

export type QaStatus = 'idle' | 'asking' | 'done' | 'error' | 'cancelled';

export interface QaState {
  status: QaStatus;
  /** The question text for the current/last ask. */
  query: string | null;
  answer: QaAnswer | null;
  error: string | null;
  /** Answer text streamed so far while `status === 'asking'`; empty in other states. */
  partial: string;
}

let state: QaState = { status: 'idle', query: null, answer: null, error: null, partial: '' };
// Bumped on every ask / cancel / reset. A resolve whose generation does not
// match is stale (cancelled or superseded) and is discarded.
let generation = 0;
const subscribers = new Set<() => void>();

function notify(): void { for (const cb of subscribers) cb(); }
function set(next: QaState): void { state = next; notify(); }

/** Read the current singleton state (non-hook; used by tests). */
export function getQaState(): QaState { return state; }

/** Fires an ask. No-op when one is already in flight; a stray call cannot
 *  double-submit. */
export async function askQuestion(query: string): Promise<void> {
  const q = query.trim();
  if (!q) return;
  if (state.status === 'asking') return;
  const gen = ++generation;
  set({ status: 'asking', query: q, answer: null, error: null, partial: '' });
  try {
    const answer = await tauri.qaAskStream(q, (delta) => {
      if (gen !== generation) return; // tokens from a cancelled/superseded ask
      set({ ...state, partial: state.partial + delta });
    });
    if (gen !== generation) return; // cancelled or superseded
    set({ status: 'done', query: q, answer, error: null, partial: '' });
  } catch (e: unknown) {
    if (gen !== generation) return;
    // Daisy Cloud not entitled → notice dialog; clear the asking state quietly.
    if (showGatewayNoticeIfNeeded(e)) {
      set({ status: 'idle', query: q, answer: null, error: null, partial: '' });
      return;
    }
    set({ status: 'error', query: q, answer: null, partial: '',
      error: String((e as { message?: unknown })?.message ?? e) });
  }
}

/** Stop waiting on the in-flight ask and discard its result. */
export function cancelQuestion(): void {
  if (state.status !== 'asking') return;
  generation++; // invalidate the pending resolve
  set({ status: 'cancelled', query: state.query, answer: null, error: null, partial: '' });
}

/** Clear back to idle (e.g. the user left Q&A mode). */
export function resetQa(): void {
  generation++;
  set({ status: 'idle', query: null, answer: null, error: null, partial: '' });
}

export function useQa(): QaState {
  const [, setTick] = useState(0);
  useEffect(() => {
    const cb = () => setTick((n) => n + 1);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return state;
}

export const __qaTestStore = {
  reset(): void {
    state = { status: 'idle', query: null, answer: null, error: null, partial: '' };
    generation = 0;
    subscribers.clear();
  },
};
