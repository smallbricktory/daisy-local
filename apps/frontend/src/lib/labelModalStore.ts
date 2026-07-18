// Tiny singleton driving whether the Speaker-label modal is open. The modal
// opens only on an explicit user action — the "Label speakers" button on the
// needs-labels toast — and this store carries that intent. Mirrors the
// recordingState / toastStore singleton pattern.
import { useEffect, useState } from 'react';

export interface LabelModalState {
  open: boolean;
  sessionId: string | null;
  title: string | null;
}

let state: LabelModalState = { open: false, sessionId: null, title: null };
const subscribers = new Set<() => void>();

function notify(): void { for (const cb of subscribers) cb(); }

/** Open the labeler for a session (from the needs-labels toast action). */
export function openLabelModal(sessionId: string, title: string | null): void {
  state = { open: true, sessionId, title };
  notify();
}

export function closeLabelModal(): void {
  if (!state.open) return;
  state = { open: false, sessionId: null, title: null };
  notify();
}

export function useLabelModal(): LabelModalState {
  const [, setTick] = useState(0);
  useEffect(() => {
    const cb = () => setTick((n) => n + 1);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return state;
}

export const __labelModalTestStore = {
  reset(): void { state = { open: false, sessionId: null, title: null }; subscribers.clear(); },
  get(): LabelModalState { return state; },
};
