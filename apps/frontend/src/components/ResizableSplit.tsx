import { ReactNode, useCallback, useRef, useState } from 'react';

export function ResizableSplit({ storageKey, top, bottom, defaultFraction = 0.5, minFraction = 0.2, maxFraction = 0.8 }: {
  storageKey: string; top: ReactNode; bottom: ReactNode; defaultFraction?: number; minFraction?: number; maxFraction?: number;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [fraction, setFraction] = useState<number>(() => {
    const v = Number(localStorage.getItem(storageKey));
    return Number.isFinite(v) && v > 0 && v < 1 ? v : defaultFraction;
  });
  const fractionRef = useRef(fraction); fractionRef.current = fraction;
  const dragging = useRef(false);

  const onMove = useCallback((clientY: number) => {
    const el = containerRef.current; if (!el) return;
    const rect = el.getBoundingClientRect();
    let f = (clientY - rect.top) / rect.height;
    f = Math.max(minFraction, Math.min(maxFraction, f));
    setFraction(f);
  }, [minFraction, maxFraction]);

  function startDrag(e: React.MouseEvent) {
    e.preventDefault(); dragging.current = true;
    const move = (ev: MouseEvent) => { if (dragging.current) onMove(ev.clientY); };
    const up = () => {
      dragging.current = false;
      localStorage.setItem(storageKey, String(fractionRef.current));
      document.removeEventListener('mousemove', move); document.removeEventListener('mouseup', up);
    };
    document.addEventListener('mousemove', move); document.addEventListener('mouseup', up);
  }

  return (
    <div className="rsplit" ref={containerRef}>
      <div className="rsplit__pane" style={{ flex: `0 0 ${fraction * 100}%` }}>{top}</div>
      <div className="rsplit__divider" role="separator" aria-orientation="horizontal" onMouseDown={startDrag} />
      <div className="rsplit__pane" style={{ flex: '1 1 0%' }}>{bottom}</div>
    </div>
  );
}
