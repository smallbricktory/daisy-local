// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One module-level mirror of a resource the backend owns and announces. It
// reads the resource on the first subscription, follows `event` after that,
// and reads it again whenever the window becomes visible. An answer that
// lands after a newer one (an event, or a write's own reply) is dropped.

import { listen } from '@tauri-apps/api/event';
import { useSyncExternalStore } from 'react';

export interface BackendStore<T> {
  /** React hook: the current value. */
  use: () => T;
  /** The current value, for code outside React. */
  get: () => T;
  /** Reads the resource again. Rejects with the read's error, except on the
   *  store's first read, which a failure leaves at its initial value. */
  refresh: () => Promise<void>;
  /** Takes a value the backend handed back (a write's reply), or one a write
   *  is about to store. */
  set: (next: T) => void;
  /** Module-level subscription for code outside React. */
  subscribe: (cb: () => void) => () => void;
  /** Returns the store to its pre-subscription state. Tests only. */
  reset: () => void;
}

export function backendStore<T>(
  event: string,
  fetch: () => Promise<T>,
  initial: T,
  same: (a: T, b: T) => boolean = Object.is,
): BackendStore<T> {
  let value = initial;
  let started: Promise<void> | null = null;
  let generation = 0;
  let watchingVisibility = false;
  const subscribers = new Set<() => void>();

  function apply(next: T): void {
    if (same(value, next)) return;
    value = next;
    for (const cb of subscribers) cb();
  }
  function set(next: T): void {
    generation++;
    apply(next);
  }
  function read(): Promise<void> {
    const mine = ++generation;
    // A fetch that throws rejects like one that fails.
    return new Promise<T>((resolve) => resolve(fetch()))
      .then((v) => { if (mine === generation) apply(v); });
  }
  function start(): Promise<void> {
    if (started) return started;
    void listen<T>(event, (e) => set(e.payload)).catch(() => {});
    started = read().catch(() => {});
    if (!watchingVisibility && typeof document !== 'undefined') {
      watchingVisibility = true;
      document.addEventListener('visibilitychange', () => {
        if (started && document.visibilityState === 'visible') void read().catch(() => {});
      });
    }
    return started;
  }
  function subscribe(cb: () => void): () => void {
    void start();
    subscribers.add(cb);
    return () => { subscribers.delete(cb); };
  }
  const get = () => value;
  return {
    use: () => useSyncExternalStore(subscribe, get),
    get,
    refresh: () => (started ? read() : start()),
    set,
    subscribe,
    reset: () => {
      value = initial;
      started = null;
      generation = 0;
      subscribers.clear();
    },
  };
}
