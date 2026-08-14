// Module-level singleton that tracks in-flight diarize calls per session and
// emits window CustomEvents; the App-level toast and the SummaryPane react
// without prop-drilling. A diarize kicked from the Participants tab survives
// navigation, with progress shown via the toast.
import { tauri, type DiarizeTrack } from '../tauri';

export type DiarizeStartDetail = { sessionId: string; title: string | null };
export type DiarizeDoneDetail = {
  sessionId: string;
  ok: boolean;
  result?: { speakers: number; segments_labeled: number };
  error?: string;
  elapsed_s: number;
};

const inFlight = new Map<string, Promise<{ speakers: number; segments_labeled: number }>>();

export function isDiarizing(sessionId: string): boolean {
  return inFlight.has(sessionId);
}

/** Fire-and-forget diarize. Returns the in-flight promise (or the existing
 *  one if a diarize for this session is already running). Emits
 *  `daisy:diarize-start` immediately and `daisy:diarize-done` when complete. */
export function runDiarize(sessionId: string, title: string | null, expectedSpeakers?: number | null, track?: DiarizeTrack | null): Promise<{ speakers: number; segments_labeled: number }> {
  const existing = inFlight.get(sessionId);
  if (existing) return existing;

  const t0 = performance.now();
  window.dispatchEvent(new CustomEvent<DiarizeStartDetail>('daisy:diarize-start', {
    detail: { sessionId, title },
  }));

  const p = tauri.diarizeSession(sessionId, expectedSpeakers, track)
    .then((result) => {
      inFlight.delete(sessionId);
      const elapsed_s = (performance.now() - t0) / 1000;
      window.dispatchEvent(new CustomEvent<DiarizeDoneDetail>('daisy:diarize-done', {
        detail: { sessionId, ok: true, result, elapsed_s },
      }));
      return result;
    })
    .catch((e: unknown) => {
      inFlight.delete(sessionId);
      const elapsed_s = (performance.now() - t0) / 1000;
      const msg = String((e as { message?: unknown })?.message ?? e);
      window.dispatchEvent(new CustomEvent<DiarizeDoneDetail>('daisy:diarize-done', {
        detail: { sessionId, ok: false, error: msg, elapsed_s },
      }));
      throw e;
    });

  inFlight.set(sessionId, p);
  return p;
}
