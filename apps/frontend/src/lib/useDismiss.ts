// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, type RefObject } from 'react';

/** How every popup in the app is dismissed: a mousedown anywhere outside it
 *  (outside every element in `ref` when it names several), and Escape. Listens only while `open`.
 *
 *  `escape` is off for a popup that answers Escape on its own control — a
 *  listbox stops the key from reaching the dialog around it. */
export function useDismiss(
  open: boolean,
  ref: RefObject<HTMLElement | null> | RefObject<HTMLElement | null>[],
  dismiss: () => void,
  { escape = true }: { escape?: boolean } = {},
): void {
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const refs = Array.isArray(ref) ? ref : [ref];
      if (!refs.some((r) => r.current?.contains(e.target as Node))) dismiss();
    };
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') dismiss(); };
    document.addEventListener('mousedown', onDown);
    if (escape) document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      if (escape) document.removeEventListener('keydown', onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, escape]);
}
