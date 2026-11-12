// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { ReactNode, useCallback, useRef, useState } from 'react';

export function ResizableSplit({ storageKey, top, bottom, defaultFraction = 0.5, minFraction = 0.2, maxFraction = 0.8, boostFraction = null }: {
  storageKey: string; top: ReactNode; bottom: ReactNode; defaultFraction?: number; minFraction?: number; maxFraction?: number;
  /** While set, the top pane grows to at least this fraction. The stored
   *  fraction is left alone; dragging the divider drops the boost until the
   *  caller clears and sets it again. */
  boostFraction?: number | null;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [fraction, setFraction] = useState<number>(() => {
    const v = Number(localStorage.getItem(storageKey));
    return Number.isFinite(v) && v > 0 && v < 1 ? v : defaultFraction;
  });
  const fractionRef = useRef(fraction); fractionRef.current = fraction;
  const [dragActive, setDragActive] = useState(false);
  const [boostDropped, setBoostDropped] = useState(false);
  // Clearing the boost arms it again. Read while rendering rather than in an
  // effect, so the pane never paints one frame at the dropped size first.
  const [lastBoost, setLastBoost] = useState(boostFraction);
  if (lastBoost !== boostFraction) {
    setLastBoost(boostFraction);
    if (boostFraction == null) setBoostDropped(false);
  }
  const shown = boostFraction != null && !boostDropped
    ? Math.min(maxFraction, Math.max(fraction, boostFraction))
    : fraction;

  const onMove = useCallback((clientY: number) => {
    const el = containerRef.current; if (!el) return;
    const rect = el.getBoundingClientRect();
    let f = (clientY - rect.top) / rect.height;
    f = Math.max(minFraction, Math.min(maxFraction, f));
    setBoostDropped(true);
    setFraction(f);
  }, [minFraction, maxFraction]);

  function startDrag(e: React.MouseEvent) {
    e.preventDefault(); setDragActive(true);
    // The listeners live for the length of the drag, so there is no second
    // flag saying whether one is under way.
    const move = (ev: MouseEvent) => onMove(ev.clientY);
    const up = () => {
      setDragActive(false);
      localStorage.setItem(storageKey, String(fractionRef.current));
      document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', up);
    };
    document.addEventListener('mousemove', move); document.addEventListener('mouseup', up);
  }

  return (
    <div className={`rsplit${dragActive ? ' rsplit--dragging' : ''}`} ref={containerRef}>
      <div className="rsplit__pane" style={{ flex: `0 0 ${shown * 100}%` }}>{top}</div>
      <div className="rsplit__divider" role="separator" aria-orientation="horizontal" onMouseDown={startDrag} />
      <div className="rsplit__pane" style={{ flex: '1 1 0%' }}>{bottom}</div>
    </div>
  );
}
