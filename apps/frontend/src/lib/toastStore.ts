// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Single toast store: one component, one stack, one set of CSS rules for
// every transient notification.

import { useEffect, useState } from 'react';

export type ToastSeverity = 'info' | 'working' | 'done' | 'warning' | 'error';

export interface ToastSpec {
  /** Stable dedup key. Reusing the same id replaces the existing toast. */
  id: string;
  severity: ToastSeverity;
  /** Primary line — e.g. "Finalizing X", "Grouping voices in X". */
  title: string;
  /** Optional secondary line under the title. */
  body?: string;
  /** 0..1 if known. Renders an indeterminate bar when undefined while
   *  severity === 'working'. */
  progress?: number;
  /** Per-stage timing string, e.g. "compressing 12s · polishing 3s". */
  timings?: string;
  /** Action buttons rendered inside the toast. */
  actions?: Array<{ label: string; onClick: () => void; primary?: boolean }>;
  /** Click anywhere on the toast (outside an action button). */
  onClick?: () => void;
  /** Auto-dismiss after this many ms. Ignored when severity === 'working'. */
  autoDismissMs?: number;
  /** Runs when the user dismisses the toast with its X. */
  onDismiss?: () => void;
}

let toasts: ToastSpec[] = [];
const subscribers = new Set<() => void>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();
// What the user dismissed by hand, per id. The repeating emitters (mic
// silence, device warnings) re-push the same id on every poll; a suppressed
// id swallows those repeats. Cleared once the content changes, or when the
// emitter itself takes the toast down.
const dismissedSignatures = new Map<string, string>();

function signature(spec: ToastSpec): string {
  return [spec.severity, spec.title, spec.body ?? ''].join('\u0000');
}

function notify(): void {
  for (const cb of subscribers) cb();
}

function scheduleAutoDismiss(spec: ToastSpec): void {
  const existing = timers.get(spec.id);
  if (existing) { clearTimeout(existing); timers.delete(spec.id); }
  if (spec.severity === 'working') return;
  const ms = spec.autoDismissMs;
  if (ms == null || ms <= 0) return;
  const t = setTimeout(() => { dismissToast(spec.id); }, ms);
  timers.set(spec.id, t);
}

export function pushToast(spec: ToastSpec): void {
  const dismissed = dismissedSignatures.get(spec.id);
  if (dismissed != null) {
    if (dismissed === signature(spec)) return;
    dismissedSignatures.delete(spec.id);
  }
  const idx = toasts.findIndex((t) => t.id === spec.id);
  if (idx >= 0) {
    toasts = [...toasts.slice(0, idx), spec, ...toasts.slice(idx + 1)];
  } else {
    toasts = [...toasts, spec];
  }
  scheduleAutoDismiss(spec);
  notify();
}

/** `byUser` marks an X-click: that exact toast stays down until its content
 *  changes. A programmatic dismiss means the condition cleared, so it lifts
 *  any standing suppression instead. */
export function dismissToast(id: string, byUser = false): void {
  const idx = toasts.findIndex((t) => t.id === id);
  if (idx < 0) {
    if (!byUser) dismissedSignatures.delete(id);
    return;
  }
  const gone = toasts[idx];
  if (byUser) dismissedSignatures.set(id, signature(gone));
  else dismissedSignatures.delete(id);
  toasts = [...toasts.slice(0, idx), ...toasts.slice(idx + 1)];
  const timer = timers.get(id);
  if (timer) { clearTimeout(timer); timers.delete(id); }
  notify();
  if (byUser) gone.onDismiss?.();
}

export function useToasts(): ToastSpec[] {
  const [snap, setSnap] = useState(toasts);
  useEffect(() => {
    const cb = () => setSnap(toasts);
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }, []);
  return snap;
}
