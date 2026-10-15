// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useLayoutEffect, useState, type CSSProperties, type RefObject } from 'react';

const GAP = 4;
const EDGE = 8;

/** Viewport placement for a popup rendered at the document root: under the
 *  anchor and as wide as it, or above it when the room below is short and
 *  the room above is larger. Follows the anchor through scrolls and resizes
 *  while `open`. */
export function useAnchoredPop(
  open: boolean,
  anchor: RefObject<HTMLElement | null>,
  maxHeight: number,
): CSSProperties | null {
  const [style, setStyle] = useState<CSSProperties | null>(null);
  useLayoutEffect(() => {
    if (!open) { setStyle(null); return; }
    const place = () => {
      const el = anchor.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      const below = window.innerHeight - r.bottom - GAP - EDGE;
      const above = r.top - GAP - EDGE;
      const down = below >= Math.min(maxHeight, 160) || below >= above;
      setStyle({
        left: r.left,
        width: r.width,
        maxHeight: Math.max(0, Math.min(maxHeight, down ? below : above)),
        ...(down ? { top: r.bottom + GAP } : { bottom: window.innerHeight - r.top + GAP }),
      });
    };
    place();
    window.addEventListener('scroll', place, true);
    window.addEventListener('resize', place);
    return () => {
      window.removeEventListener('scroll', place, true);
      window.removeEventListener('resize', place);
    };
  }, [open, anchor, maxHeight]);
  return style;
}
