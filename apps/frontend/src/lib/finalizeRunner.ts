// Headless finalize/cascade runner. Kicks the backend work (stop_recording +
// recording_finalize_and_summarize, the regen variants, and resume-finalize).
// The session-status toasts display progress by polling the
// finalize.status.json sidecar via the lifecycle store; this module starts
// the work.
//
// One in-flight run per session id (dedup).
import { useEffect, useState } from 'react';
import { tauri } from '../tauri';
import type { JobKind } from './sessionPhase';
import { runDiarize } from './diarizeBus';
import { pushToast, dismissToast } from './toastStore';
import { showGatewayNoticeIfNeeded } from './gatewayNotice';

// In-flight finalize/summarize jobs, keyed by session id. Observable; the UI
// (e.g. SummaryPane's Generate/Regenerate buttons) reflects "a job is running
// for this session".
const inFlight = new Set<string>();
const inFlightSubs = new Set<() => void>();

function addInFlight(id: string): void {
  inFlight.add(id);
  for (const cb of inFlightSubs) cb();
}
function removeInFlight(id: string): void {
  inFlight.delete(id);
  for (const cb of inFlightSubs) cb();
}

/** True while any finalize/summarize job for `sessionId` is running. */
export function isFinalizing(sessionId: string): boolean {
  return inFlight.has(sessionId);
}

/** React hook: re-renders when `sessionId`'s in-flight state flips. */
export function useFinalizing(sessionId: string): boolean {
  const [running, setRunning] = useState(() => inFlight.has(sessionId));
  useEffect(() => {
    const cb = () => setRunning(inFlight.has(sessionId));
    cb();
    inFlightSubs.add(cb);
    return () => { inFlightSubs.delete(cb); };
  }, [sessionId]);
  return running;
}

/**
 * Run a finalize job for `sessionId`. `kind`:
 *  - 'cascade' (default): stop the recording (unless already stopped) then run
 *    the full finalize+transcribe+dedup+diarize+summary cascade. Pauses at the
 *    speaker-label gate (NeedsLabels) — the session-status toast surfaces that
 *    via the sidecar's `awaiting-labels` stage; the user resumes via
 *    `resumeFinalize`.
 *  - 'regen-summary': re-run just the AI summary; falls back to a full cascade
 *    if no transcript exists yet.
 *  - 'regen-transcript': re-transcribe + dedup + re-diarize against existing
 *    chunk WAVs.
 *
 * Fire-and-forget. Progress reaches the UI through the sidecar (cascade); the
 * caller arms `beginFinalizeWatch` first and the lifecycle store polls.
 * Errors are logged (not rethrown): nothing awaits this, and the
 * error/interrupted state is driven by the sidecar.
 *
 * `alreadyStopped` skips the stop_recording prelude (manual regen on a
 * session that isn't the active recording).
 */
export async function runFinalize(
  sessionId: string,
  kind: JobKind = 'cascade',
  alreadyStopped = false,
  promptId?: string,
): Promise<void> {
  if (inFlight.has(sessionId)) return;
  addInFlight(sessionId);
  try {
    if (kind === 'regen-summary') {
      // regen-summary writes no finalize sidecar; a toast (start →
      // done/error) surfaces progress.
      const toastId = `regen-summary:${sessionId}`;
      pushToast({ id: toastId, severity: 'working', title: 'Generating summary…' });
      try {
        try {
          await tauri.summaryRegenerate(sessionId, promptId);
        } catch (e: unknown) {
          const msg = String((e as { message?: unknown })?.message ?? e);
          // No transcript on disk yet → escalate to the full cascade.
          if (!/transcript/i.test(msg)) throw e;
          await tauri.recordingFinalizeAndSummarize({ session_id: sessionId, skip_label_gate: true });
        }
        pushToast({ id: toastId, severity: 'done', title: 'Summary ready', autoDismissMs: 4000 });
      } catch (e: unknown) {
        // Daisy Cloud not entitled → notice dialog, no error toast.
        if (showGatewayNoticeIfNeeded(e)) {
          dismissToast(toastId);
          throw e;
        }
        pushToast({
          id: toastId,
          severity: 'error',
          title: 'Summary generation failed',
          body: String((e as { message?: unknown })?.message ?? e),
          autoDismissMs: 8000,
          dismissible: true,
        });
        throw e;
      }
    } else if (kind === 'regen-transcript') {
      // No sidecar for this path; transcription can run for many minutes. A
      // toast surfaces progress (start → hand-off to the diarize toast / error).
      const tToast = `regen-transcript:${sessionId}`;
      pushToast({ id: tToast, severity: 'working', title: 'Re-transcribing…' });
      try {
        await tauri.transcribe({ session_id: sessionId });
        await tauri.dedup({ session_id: sessionId });
        // Stamps finalized; any lingering "unfinalized" surface for a
        // recovered orphan clears. Idempotent.
        await tauri.markSessionComplete(sessionId).catch(() => { /* non-fatal */ });
        // The diarize bus owns the next toast; clear ours as we hand off.
        dismissToast(tToast);
        // Re-diarize: retranscribe reset cluster assignments. Fire-and-forget via
        // the diarize bus (its own toast + ParticipantsTab refresh).
        void runDiarize(sessionId, null).catch(() => { /* surfaced via toast */ });
      } catch (e: unknown) {
        pushToast({
          id: tToast,
          severity: 'error',
          title: 'Re-transcription failed',
          body: String((e as { message?: unknown })?.message ?? e),
          autoDismissMs: 8000,
          dismissible: true,
        });
        throw e;
      }
    } else if (kind === 'polish-transcript') {
      // On-demand transcript polish. No sidecar — a toast surfaces progress.
      // The backend `polish` command re-renders transcript.md + emits
      // library:changed; an open SummaryPane reloads the polished text.
      const pToast = `polish:${sessionId}`;
      pushToast({ id: pToast, severity: 'working', title: 'Polishing transcript…' });
      try {
        const sum = await tauri.polish({ session_id: sessionId });
        if (!sum || sum.batches === 0) {
          // Nothing ran — polish_impl no-ops with no AI provider configured
          // or an empty transcript.
          pushToast({
            id: pToast, severity: 'info',
            title: 'Nothing to polish',
            body: 'No AI provider is configured, or the transcript is empty.',
            autoDismissMs: 6000, dismissible: true,
          });
        } else {
          const failed = sum.failed_batches ? ` · ${sum.failed_batches} batch${sum.failed_batches === 1 ? '' : 'es'} failed` : '';
          pushToast({
            id: pToast, severity: 'done',
            title: 'Transcript polished',
            body: `${sum.segments_polished} segment${sum.segments_polished === 1 ? '' : 's'} cleaned${failed}`,
            autoDismissMs: 5000, dismissible: true,
          });
        }
      } catch (e: unknown) {
        pushToast({
          id: pToast, severity: 'error',
          title: 'Polish failed',
          body: String((e as { message?: unknown })?.message ?? e),
          autoDismissMs: 8000, dismissible: true,
        });
        throw e;
      }
    } else {
      // Full cascade. Stops the live recording first (unless already
      // stopped), then finalizes. The sidecar drives the session-status
      // toasts through the stages.
      if (!alreadyStopped) {
        await tauri.stopRecording().catch(() => { /* slot already clear */ });
      }
      await tauri.recordingFinalizeAndSummarize({ session_id: sessionId });
      // NeedsLabels is reflected by the sidecar's awaiting-labels stage; the
      // session-status toast shows the label gate.
    }
  } catch (e) {
    // The sidecar carries the user-facing error/interrupted state. Log for
    // diagnostics; do not rethrow (no one awaits this).
    console.error(`finalize (${kind}) for ${sessionId} failed:`, e);
  } finally {
    removeInFlight(sessionId);
  }
}

/**
 * Resume a finalize that paused at the speaker-label gate (after the user named
 * the new speakers). Runs the post-gate tail (summary → chapters → compress).
 */
export async function resumeFinalize(sessionId: string): Promise<void> {
  if (inFlight.has(sessionId)) return;
  addInFlight(sessionId);
  try {
    await tauri.recordingResumeFinalize({ session_id: sessionId });
  } catch (e) {
    console.error(`resume-finalize for ${sessionId} failed:`, e);
  } finally {
    removeInFlight(sessionId);
  }
}

/** Test-only escape hatch to reset the in-flight guard. */
export const __resetFinalizeRunner = (): void => {
  inFlight.clear();
  for (const cb of inFlightSubs) cb();
};
