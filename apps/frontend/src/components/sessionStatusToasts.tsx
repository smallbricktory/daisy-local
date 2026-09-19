// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Projects the session-lifecycle phase onto the unified toast stack. The
// lifecycle store (sessionLifecycle.ts) is the engine (recording snapshot +
// finalize sidecar poll); this component is its renderer.
//
// One toast id (`session-status`): the store only ever holds a single phase
// at a time. pushToast replaces by id; each phase transition updates the
// same toast in place (incl. the finalize progress bar).
import { useEffect, useRef, useState } from 'react';
import { useSessionPhase, dismissPhase, beginFinalizeWatch } from '../lib/sessionLifecycle';
import { resumeFinalize, runFinalize } from '../lib/finalizeRunner';
import { useAiProviderStatus } from '../lib/aiProviderStatus';
import { FINALIZE_STAGES, type FinalizeStage } from '../lib/sessionPhase';
import { pushToast, dismissToast } from '../lib/toastStore';
import { tauri } from '../tauri';
import { openLabelModal } from '../lib/labelModalStore';

const STAGE_LABEL: Record<FinalizeStage, string> = {
  finalizing: 'Finalizing audio', 'echo-cancelling': 'Removing speaker bleed',
  transcribing: 'Transcribing', deduping: 'Cleaning up transcript',
  polishing: 'Polishing transcript', 'awaiting-labels': 'New speakers',
  summarizing: 'Writing summary', chaptering: 'Writing chapters',
  compressing: 'Compressing audio', resuming: 'Resuming',
  done: 'Ready', error: 'Failed',
};

export const SESSION_STATUS_TOAST_ID = 'session-status';

export interface SessionStatusActions {
  /** Recording/paused → the recording screen (real Pause/Finish controls). */
  onOpenRecording: () => void;
  /** A finished session → its library entry. */
  onOpenSession: (sessionId: string) => void;
  /** Interrupted → re-run the missing finalize stages. */
  onResume: (sessionId: string, title: string | null) => void;
  /** "Copy prompt" for a session with no AI provider (transcript lives in
   *  SummaryPane). */
  onCopyPrompt: (sessionId: string) => void;
  /** "Want AI?" → open the get-a-key help page. Offered only when no
   *  provider is configured. */
  onWantAi: () => void;
}

/** Headless: subscribes to the lifecycle phase and keeps the `session-status`
 *  toast in sync. Renders nothing. */
export function SessionStatusToasts(actions: SessionStatusActions): null {
  const phase = useSessionPhase();
  // "No summary on disk" and "no AI configured" are different states; the
  // done toast's call to action comes from the provider status.
  const ai = useAiProviderStatus();
  // Holds the latest action handlers outside the effect dependencies. App
  // passes a fresh object each render; the effect re-runs on phase only.
  const aRef = useRef(actions);
  aRef.current = actions;

  // When finalize was given up after repeated crashes (the recovery cap,
  // e.g. OOM on a low-memory box), fetches the friendly reason; the toast
  // shows it and offers a Retry that resets the cap. null = not a given-up
  // session.
  const [recovery, setRecovery] = useState<{ failed: boolean; reason?: string | null } | null>(null);
  useEffect(() => {
    if (phase.kind !== 'interrupted') {
      setRecovery(null);
      return;
    }
    let live = true;
    tauri
      .finalizeRecovery(phase.sessionId)
      .then((r) => { if (live) setRecovery(r.failed ? r : null); })
      .catch(() => { if (live) setRecovery(null); });
    return () => { live = false; };
  }, [phase]);

  useEffect(() => {
    const a = aRef.current;
    switch (phase.kind) {
      case 'idle':
      case 'hidden':
        dismissToast(SESSION_STATUS_TOAST_ID);
        break;

      case 'recording': {
        const paused = phase.snap.state === 'paused';
        const lm = phase.snap.live_mode_label;
        pushToast({
          id: SESSION_STATUS_TOAST_ID,
          severity: 'working',
          title: paused ? 'Paused' : 'Recording',
          body: lm && lm !== 'off' ? `Transcribing live: ${lm}` : undefined,
          actions: [{ label: 'Open', onClick: a.onOpenRecording }],
        });
        break;
      }

      case 'finalizing': {
        const idx = FINALIZE_STAGES.indexOf(phase.stage);
        const n = idx < 0 ? 1 : idx + 1;
        pushToast({
          id: SESSION_STATUS_TOAST_ID,
          severity: 'working',
          title: `Finishing ${phase.title ?? 'recording'}`,
          body: `${STAGE_LABEL[phase.stage]} · step ${n} of ${FINALIZE_STAGES.length}`,
          progress: phase.progress,
        });
        break;
      }

      case 'needs-labels': {
        const n = phase.clusters.length;
        const sid = phase.sessionId;
        const title = phase.title;
        // "Later", and the X, defer labeling and still kick the summary tail
        // with whatever labels exist (mirrors the modal's Finish later).
        const later = () => {
          beginFinalizeWatch(sid, title);
          void resumeFinalize(sid);
          dismissPhase();
        };
        pushToast({
          id: SESSION_STATUS_TOAST_ID,
          severity: 'warning',
          title: `${phase.title ?? 'Recording'} is ready`,
          body: `${n} new voice${n === 1 ? '' : 's'} to label`,
          actions: [
            { label: 'Label speakers', primary: true, onClick: () => openLabelModal(sid, title) },
            { label: 'Later', onClick: later },
          ],
          onDismiss: later,
        });
        break;
      }

      case 'done': {
        const summarized = phase.summary.hadAi;
        const sid = phase.sessionId;
        const acts = [{ label: 'Open', onClick: () => a.onOpenSession(sid) }];
        let body = 'Transcript · summary';
        if (!summarized) {
          if (ai.configured) {
            body = 'Transcribed · summary not written';
            acts.push({
              label: 'Generate summary',
              onClick: () => { void runFinalize(sid, 'regen-summary', true); },
            });
          } else if (ai.state === 'VaultLocked') {
            body = 'Transcribed · unlock the vault to summarize';
          } else if (ai.state === 'Unreachable') {
            body = `Transcribed · ${ai.provider ?? 'the AI provider'} is unreachable`;
          } else if (ai.state === 'Unsupported') {
            body = `Transcribed · ${ai.hint ?? 'the built-in model needs more memory on this computer'}`;
            acts.push({ label: 'Copy prompt', onClick: () => a.onCopyPrompt(sid) });
          } else if (ai.state === 'loading') {
            body = 'Transcribed';
          } else {
            body = 'Transcribed · no AI summary';
            acts.push({ label: 'Copy prompt', onClick: () => a.onCopyPrompt(sid) });
            acts.push({ label: 'Want AI? →', onClick: () => a.onWantAi() });
          }
        }
        pushToast({
          id: SESSION_STATUS_TOAST_ID,
          severity: 'done',
          title: `${phase.title ?? 'Recording'} ${summarized ? 'ready ✓' : 'transcribed ✓'}`,
          body,
          actions: acts,
          autoDismissMs: 8000,
        });
        break;
      }

      case 'interrupted': {
        const sid = phase.sessionId;
        const title = phase.title ?? null;
        // Given up after repeated finalize crashes → plain-language message +
        // a Retry that resets the cap.
        if (recovery?.failed) {
          pushToast({
            id: SESSION_STATUS_TOAST_ID,
            severity: 'error',
            title: `Couldn't finish ${phase.title ?? 'recording'}`,
            body: `${recovery.reason ?? "We couldn't finish processing this recording."} Your audio is saved.`,
            actions: [
              { label: 'Retry', primary: true, onClick: () => void tauri.retryFinalize(sid) },
              { label: 'Keep as-is', onClick: () => dismissPhase() },
            ],
          });
          break;
        }
        pushToast({
          id: SESSION_STATUS_TOAST_ID,
          severity: 'error',
          title: `${phase.title ?? 'Recording'} didn't finish`,
          body: `Stopped at: ${STAGE_LABEL[phase.lastStage]}`,
          actions: [
            { label: 'Resume', primary: true, onClick: () => a.onResume(sid, title) },
            { label: 'Keep as-is', onClick: () => dismissPhase() },
          ],
        });
        break;
      }
    }
  }, [phase, recovery, ai]);

  return null;
}
