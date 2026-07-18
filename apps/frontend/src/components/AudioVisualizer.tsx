/**
 * AudioVisualizer — scrolling mic-activity bars for the captions-off
 * recording view.
 *
 * Purely presentational: driven by the `level` prop (0..1, the recording's
 * own mic peak from the `recording:mic-level` event) — opens no capture of
 * its own. Each animation frame pushes the latest level into a ring buffer
 * and draws it as vertically-centered mirrored bars, newest on the right.
 * A stale input (no prop change for >300 ms, e.g. recording paused) decays
 * to the quiet floor instead of freezing at the last value.
 *
 * With `prefers-reduced-motion: reduce` the scroll is disabled and a single
 * centered bar shows the current level.
 */

import { useEffect, useRef } from 'react';

interface Props {
  /** Mic level 0..1. */
  level: number;
  width?: number;
  height?: number;
}

const BAR_W = 2;
const BAR_GAP = 1;
/** Minimum bar height (px) so the strip reads as live even in silence. */
const QUIET_FLOOR = 2;
/** Prop staleness after which the drawn level decays toward zero. */
const STALE_MS = 300;
const DECAY_PER_FRAME = 0.88;

export function AudioVisualizer({ level, width = 380, height = 56 }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const levelRef = useRef(level);
  const lastPropAt = useRef(performance.now());

  levelRef.current = Math.min(1, Math.max(0, level));
  lastPropAt.current = performance.now();

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    if (!canvas || !ctx) return;

    const dpr = window.devicePixelRatio || 1;
    canvas.width = width * dpr;
    canvas.height = height * dpr;
    ctx.scale(dpr, dpr);

    const styles = getComputedStyle(document.documentElement);
    const record = styles.getPropertyValue('--record').trim() || '#D7263D';
    const track = styles.getPropertyValue('--frost-deep').trim() || '#e4dccb';

    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const bars = Math.floor(width / (BAR_W + BAR_GAP));
    const history = new Float32Array(bars);
    let head = 0;
    let drawn = 0;
    let raf: number | null = null;

    function frame() {
      if (!ctx) {
        return;
      }
      // Decay when the prop has gone stale (paused / muted recording).
      const stale = performance.now() - lastPropAt.current > STALE_MS;
      drawn = stale ? drawn * DECAY_PER_FRAME : levelRef.current;

      ctx.clearRect(0, 0, width, height);

      if (reduced) {
        // Static single bar of the current level, no scroll.
        const h = Math.max(QUIET_FLOOR, drawn * height);
        ctx.fillStyle = record;
        ctx.fillRect(0, (height - h) / 2, width, h);
        raf = requestAnimationFrame(frame);
        return;
      }

      history[head] = drawn;
      head = (head + 1) % bars;

      ctx.fillStyle = track;
      ctx.fillRect(0, height / 2 - 0.5, width, 1);

      ctx.fillStyle = record;
      for (let i = 0; i < bars; i++) {
        const v = history[(head + i) % bars];
        const h = Math.max(QUIET_FLOOR, v * height);
        ctx.fillRect(i * (BAR_W + BAR_GAP), (height - h) / 2, BAR_W, h);
      }
      raf = requestAnimationFrame(frame);
    }
    raf = requestAnimationFrame(frame);

    return () => {
      if (raf !== null) cancelAnimationFrame(raf);
    };
  }, [width, height]);

  return (
    <canvas
      ref={canvasRef}
      role="img"
      aria-label="Microphone activity"
      style={{ width, height, display: 'block', margin: '14px auto 0' }}
    />
  );
}
