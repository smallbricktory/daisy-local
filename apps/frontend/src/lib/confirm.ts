// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Imperative confirm() — promise-based API that replaces window.confirm /
// window.alert across the app. Each asking webview (App.tsx, the mini
// window) mounts one <GlobalConfirm/> that subscribes to this store.
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
  /** A second accepting button next to the confirm one; `choose()` reports
   *  it as 'alternate'. */
  alternateLabel?: string;
}

export type ConfirmResult = 'confirm' | 'alternate' | 'cancel';

interface PendingConfirm {
  spec: ConfirmSpec;
  resolve: (result: ConfirmResult) => void;
}

let pending: PendingConfirm | null = null;
const subscribers = new Set<() => void>();

function notify(): void { for (const cb of subscribers) cb(); }

export function choose(spec: ConfirmSpec): Promise<ConfirmResult> {
  return new Promise((resolve) => {
    if (pending) {
      // A new request cancels the pending one.
      pending.resolve('cancel');
    }
    pending = { spec, resolve };
    notify();
  });
}

export function confirm(spec: ConfirmSpec): Promise<boolean> {
  return choose(spec).then((r) => r === 'confirm');
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

export function resolvePending(result: ConfirmResult): void {
  const p = pending;
  pending = null;
  notify();
  if (p) p.resolve(result);
}
