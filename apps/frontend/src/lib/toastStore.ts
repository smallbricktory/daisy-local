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
  /** Show a close × button. */
  dismissible?: boolean;
  /** Auto-dismiss after this many ms. Ignored when severity === 'working'. */
  autoDismissMs?: number;
}

let toasts: ToastSpec[] = [];
const subscribers = new Set<() => void>();
const timers = new Map<string, ReturnType<typeof setTimeout>>();

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
  const idx = toasts.findIndex((t) => t.id === spec.id);
  if (idx >= 0) {
    toasts = [...toasts.slice(0, idx), spec, ...toasts.slice(idx + 1)];
  } else {
    toasts = [...toasts, spec];
  }
  scheduleAutoDismiss(spec);
  notify();
}

export function updateToast(id: string, partial: Partial<ToastSpec>): void {
  const idx = toasts.findIndex((t) => t.id === id);
  if (idx < 0) return;
  const merged = { ...toasts[idx], ...partial };
  toasts = [...toasts.slice(0, idx), merged, ...toasts.slice(idx + 1)];
  scheduleAutoDismiss(merged);
  notify();
}

export function dismissToast(id: string): void {
  const idx = toasts.findIndex((t) => t.id === id);
  if (idx < 0) return;
  toasts = [...toasts.slice(0, idx), ...toasts.slice(idx + 1)];
  const timer = timers.get(id);
  if (timer) { clearTimeout(timer); timers.delete(id); }
  notify();
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
