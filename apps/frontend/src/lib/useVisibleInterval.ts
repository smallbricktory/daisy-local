import { useEffect, useRef, useState } from 'react';

/**
 * Visibility-aware interval. Same shape as `useEffect(() => setInterval(...))`
 * but pauses the timer whenever `document.visibilityState === 'hidden'` and
 * resumes on `visibilitychange`.
 *
 * The callback identity is held in a ref; changing it does not reset the
 * interval.
 *
 * Pass `enabled = false` to gate without manually nulling out `ms`.
 */
export function useVisibleInterval(
  fn: () => void,
  ms: number,
  enabled: boolean = true,
) {
  const fnRef = useRef(fn);
  useEffect(() => { fnRef.current = fn; }, [fn]);

  // Visibility is tracked separately; the effect re-runs (clearing/resetting
  // the timer) when the tab is hidden / shown.
  const visible = useDocumentVisibility();

  useEffect(() => {
    if (!enabled || !visible) return;
    const id = window.setInterval(() => fnRef.current(), ms);
    return () => window.clearInterval(id);
  }, [ms, enabled, visible]);
}

/** Returns true when document.visibilityState is 'visible'. SSR-safe (defaults true). */
export function useDocumentVisibility(): boolean {
  const [visible, setVisible] = useState(() => {
    if (typeof document === 'undefined') return true;
    return document.visibilityState !== 'hidden';
  });
  useEffect(() => {
    if (typeof document === 'undefined') return;
    const onChange = () => setVisible(document.visibilityState !== 'hidden');
    document.addEventListener('visibilitychange', onChange);
    return () => document.removeEventListener('visibilitychange', onChange);
  }, []);
  return visible;
}
