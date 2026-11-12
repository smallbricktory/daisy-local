// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';

/** Runs `handler` with the payload whenever the backend emits `event`. The
 * handler is read fresh at each event, so a caller that rebuilds it stays
 * subscribed; a subscription that resolves after unmount drops itself. */
export function useBackendEvent<T = unknown>(event: string, handler: (payload: T) => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    void listen<T>(event, (e) => {
      if (live) ref.current(e.payload);
    }).then((fn) => {
      if (live) unlisten = fn;
      else fn();
    });
    return () => {
      live = false;
      unlisten?.();
    };
  }, [event]);
}
