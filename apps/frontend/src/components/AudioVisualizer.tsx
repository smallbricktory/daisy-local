/**
 * AudioVisualizer — bouncing mic-activity meter for the captions-off
 * recording view.
 *
 * Purely presentational: driven by the `level` prop (0..1, the recording's
 * own mic peak from the `recording:mic-level` event) — opens no capture of
 * its own. Bars sit in fixed positions; every frame an envelope follows the
 * level with instant attack and an exponential decay, and each bar renders
 * envelope × its fixed weight (a center-heavy dome with deterministic
 * jitter), mirrored around the midline — the whole shape bounces with the
 * voice. Each bar also remembers its recent maximum: a peak dot is left at
 * that height and fades out over ~a second — a classic peak-hold meter. A
 * stale input (no prop change for >300 ms, e.g. recording paused) reads as
 * silence and the meter settles to the quiet floor.
 *
 * With `prefers-reduced-motion: reduce` the decay animation is disabled and
 * a single centered bar shows the current level directly.
 */

import { useEffect, useRef } from 'react';

interface Props {
  /** Mic level 0..1. */
  level: number;
  width?: number;
  height?: number;
}

const BAR_W = 3;
const BAR_GAP = 2;
/** Minimum bar height (px) in silence. */
const QUIET_FLOOR = 2;
/** Prop staleness after which the input reads as silence. */
const STALE_MS = 300;
/** Per-frame envelope decay (~350 ms fall from full at 60 fps). */
const DECAY_PER_FRAME = 0.93;
/** Per-frame peak-dot alpha fade (~1 s to invisible at 60 fps). */
const PEAK_FADE_PER_FRAME = 0.95;
/** Peaks below this bar height (px) leave no dot. */
const PEAK_MIN_PX = 8;

/** Fixed per-bar weight: a center-heavy dome with deterministic jitter. */
function barWeight(i: number, bars: number): number {
  const dome = Math.sin((Math.PI * (i + 0.5)) / bars) ** 0.7;
  const jitter = 0.75 + 0.25 * Math.abs(Math.sin(i * 12.9898) * 43758.5453 % 1);
  return 0.25 + 0.75 * dome * jitter;
}

export function AudioVisualizer({ level, width = 380, height = 112 }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const levelRef = useRef(level);
  const lastPropAt = useRef(performance.now());

  // Staleness arms on value change only, not on re-render.
  const clamped = Math.min(1, Math.max(0, level));
  if (clamped !== levelRef.current) {
    levelRef.current = clamped;
    lastPropAt.current = performance.now();
  }

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext('2d');
    if (!canvas || !ctx) return;

    const dpr = window.devicePixelRatio || 1;
    canvas.width = width * dpr;
    canvas.height = height * dpr;
    ctx.scale(dpr, dpr);

    const styles = getComputedStyle(document.documentElement);
    const green = styles.getPropertyValue('--success').trim() || '#1E8E3E';
    const track = styles.getPropertyValue('--frost-deep').trim() || '#e4dccb';

    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    const bars = Math.floor(width / (BAR_W + BAR_GAP));
    const weights = Float32Array.from({ length: bars }, (_, i) => barWeight(i, bars));
    const peakPx = new Float32Array(bars);
    const peakAlpha = new Float32Array(bars);
    let envelope = 0;
    let raf: number | null = null;

    function frame() {
      if (!ctx) {
        return;
      }
      const stale = performance.now() - lastPropAt.current > STALE_MS;
      const input = stale ? 0 : levelRef.current;
      // Instant attack, exponential decay.
      envelope = Math.max(input, envelope * DECAY_PER_FRAME);

      ctx.clearRect(0, 0, width, height);

      if (reduced) {
        // Single bar of the current level, no decay animation.
        const h = Math.max(QUIET_FLOOR, input * height);
        ctx.fillStyle = green;
        ctx.fillRect(0, (height - h) / 2, width, h);
        raf = requestAnimationFrame(frame);
        return;
      }

      ctx.fillStyle = track;
      ctx.fillRect(0, height / 2 - 0.5, width, 1);

      ctx.fillStyle = green;
      for (let i = 0; i < bars; i++) {
        const h = Math.max(QUIET_FLOOR, envelope * weights[i] * height);
        ctx.fillRect(i * (BAR_W + BAR_GAP), (height - h) / 2, BAR_W, h);
        // Peak-hold: a new maximum re-arms the dot at full opacity; otherwise
        // it stays put and fades.
        if (h > peakPx[i] && h > PEAK_MIN_PX) {
          peakPx[i] = h;
          peakAlpha[i] = 1;
        } else {
          peakAlpha[i] *= PEAK_FADE_PER_FRAME;
          if (peakAlpha[i] < 0.04) { peakAlpha[i] = 0; peakPx[i] = 0; }
        }
      }
      // Dots draw in a second pass, over all bar fills.
      for (let i = 0; i < bars; i++) {
        if (peakAlpha[i] === 0) continue;
        const x = i * (BAR_W + BAR_GAP);
        const yTop = (height - peakPx[i]) / 2 - 4;
        ctx.globalAlpha = peakAlpha[i];
        ctx.fillRect(x, yTop, BAR_W, 2);
        ctx.fillRect(x, height - yTop - 2, BAR_W, 2);
        ctx.globalAlpha = 1;
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
