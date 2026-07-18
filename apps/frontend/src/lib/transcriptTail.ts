// Computes the transcript delta to feed the in-call chat: the finalized lines
// added since the last cursor. Pure (no DOM / store access).
import { isPauseMarker, type TurnEntry } from '../liveTranscript';

export interface TranscriptTail {
  /** Joined, speaker-labelled lines. Empty when nothing new. */
  text: string;
  /** end_ms of the last included line (or the input cursor when none). */
  endMs: number;
}

/**
 * Finalized turns with `end_ms > cursorMs`, labelled by track ("Me" = your mic,
 * "Others" = everyone on the system track). Pause markers and interim turns are
 * skipped.
 */
export function transcriptTailSince(finals: TurnEntry[], cursorMs: number): TranscriptTail {
  const lines: string[] = [];
  let endMs = cursorMs;
  for (const e of finals) {
    if (isPauseMarker(e)) continue;
    if (e.isInterim) continue;
    if (e.end_ms <= cursorMs) continue;
    const who = e.track === 'mic' ? 'Me' : 'Others';
    const text = e.text.trim();
    if (text) lines.push(`${who}: ${text}`);
    if (e.end_ms > endMs) endMs = e.end_ms;
  }
  return { text: lines.join('\n'), endMs };
}
