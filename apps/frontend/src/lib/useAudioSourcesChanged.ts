import { useEffect, useRef } from 'react';
import { listen } from '@tauri-apps/api/event';

/** Runs `handler` whenever the backend announces a device-topology change
 * (`audio-sources-changed`). Callers re-fetch their source list in the
 * handler; the event carries no payload. */
export function useAudioSourcesChanged(handler: () => void): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    let live = true;
    let unlisten: (() => void) | undefined;
    void listen('audio-sources-changed', () => {
      if (live) ref.current();
    }).then((fn) => {
      if (live) unlisten = fn;
      else fn();
    });
    return () => {
      live = false;
      unlisten?.();
    };
  }, []);
}
