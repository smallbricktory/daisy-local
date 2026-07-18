import { useCallback, useEffect, useRef, useState } from 'react';

interface Offset { dx: number; dy: number }

/**
 * Lightweight drag offset for a fixed-positioned element. Returns a
 * `transform` style (applied on top of the element's CSS anchor) plus an
 * `onPointerDown` handler to attach to a drag handle. The offset persists
 * to localStorage under `storageKey`; the element stays where the user
 * parked it across reloads.
 *
 * Click vs drag: the handler only starts dragging after the pointer moves
 * past a small threshold; attached to a clickable element, it does not eat
 * the click.
 */
export function useDraggable(storageKey: string) {
  const [offset, setOffset] = useState<Offset>(() => {
    try {
      const raw = localStorage.getItem(storageKey);
      if (raw) return JSON.parse(raw) as Offset;
    } catch { /* ignore */ }
    return { dx: 0, dy: 0 };
  });

  const drag = useRef<{ startX: number; startY: number; baseDx: number; baseDy: number } | null>(null);
  // Whether the last gesture actually moved (past the click/drag threshold).
  // Persists after pointerup; the click handler that fires next checks it
  // (`drag.current` is already null by then). Reset on the next pointerdown.
  const movedRef = useRef(false);

  // Mirrors the latest offset; the drag handlers read it without being
  // recreated (and re-subscribing the window listeners) on every move.
  const offsetRef = useRef(offset);
  offsetRef.current = offset;

  // Stable — reads the current offset from the ref; it never changes
  // identity and the listener effect below subscribes exactly once.
  const onPointerDown = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return; // primary pointer only
    (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
    movedRef.current = false;
    drag.current = {
      startX: e.clientX,
      startY: e.clientY,
      baseDx: offsetRef.current.dx,
      baseDy: offsetRef.current.dy,
    };
  }, []);

  useEffect(() => {
    function move(e: PointerEvent) {
      const d = drag.current;
      if (!d) return;
      if (Math.abs(e.clientX - d.startX) > 3 || Math.abs(e.clientY - d.startY) > 3) {
        movedRef.current = true;
      }
      setOffset({ dx: d.baseDx + (e.clientX - d.startX), dy: d.baseDy + (e.clientY - d.startY) });
    }
    function up() {
      if (!drag.current) return;
      drag.current = null;
      try { localStorage.setItem(storageKey, JSON.stringify(offsetRef.current)); } catch { /* ignore */ }
    }
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    return () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
  }, [storageKey]);

  // True if the most recent gesture was a drag (not a click) — let a clickable
  // handle suppress its onClick after a drag.
  const wasDragged = useCallback(() => movedRef.current, []);

  return {
    style: { transform: `translate(${offset.dx}px, ${offset.dy}px)` } as React.CSSProperties,
    onPointerDown,
    wasDragged,
  };
}
