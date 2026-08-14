// Imperative confirm() — promise-based API that replaces window.confirm /
// window.alert across the app. Single instance lives in App.tsx as
// <GlobalConfirm/> which subscribes to this store.
//
// Usage:
//   const ok = await confirm({ title: 'Delete?', body: 'Are you sure?', confirmLabel: 'Delete', danger: true });
//   if (!ok) return;
//
//   await alert({ title: 'Saved.', body: 'Your changes are stored.' });

import { useEffect, useState } from 'react';

export interface ConfirmSpec {
  title: string;
  body: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  typedConfirm?: string;
}

interface PendingConfirm {
  spec: ConfirmSpec;
  resolve: (ok: boolean) => void;
}

let pending: PendingConfirm | null = null;
const subscribers = new Set<() => void>();

function notify(): void { for (const cb of subscribers) cb(); }

export function confirm(spec: ConfirmSpec): Promise<boolean> {
  return new Promise((resolve) => {
    if (pending) {
      // Resolve the older one as cancel rather than queueing — the UX of
      // stacked modals is worse than the rare case of dropping one.
      pending.resolve(false);
    }
    pending = { spec, resolve };
    notify();
  });
}

/** Alert-only variant — single-button OK dialog. Promise resolves when
 *  the user dismisses it. */
export function alert(spec: { title: string; body: string; okLabel?: string }): Promise<void> {
  return confirm({
    title: spec.title,
    body: spec.body,
    confirmLabel: spec.okLabel ?? 'OK',
    cancelLabel: '__hidden__', // sentinel — renderer hides cancel
  }).then(() => undefined);
}

export function usePendingConfirm(): PendingConfirm | null {
  const [snap, setSnap] = useState(pending);
  useEffect(() => {
    const cb = () => setSnap(pending);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return snap;
}

export function resolvePending(ok: boolean): void {
  const p = pending;
  pending = null;
  notify();
  if (p) p.resolve(ok);
}
