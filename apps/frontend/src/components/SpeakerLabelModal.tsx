// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useState } from 'react';
import { SpeakerLabeler, speakerNeedsReview } from './SpeakerLabeler';
import { dismissPhase, beginFinalizeWatch } from '../lib/sessionLifecycle';
import { useLabelModal, closeLabelModal } from '../lib/labelModalStore';
import { resumeFinalize } from '../lib/finalizeRunner';
import { subscribeToLibrary } from '../lib/libraryEvents';
import { tauri, type SessionSpeaker, type RecordingKind } from '../tauri';

/** Pure scrim shell - tested directly. Click the scrim (not the inner card)
 *  to close. */
export function SpeakerLabelModalView({ open, onClose, children }: {
  open: boolean; onClose: () => void; children: React.ReactNode;
}) {
  if (!open) return null;
  return (
    <div data-testid="sstat-scrim"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      className="modal-backdrop">
      <div data-testid="sstat-modal" onClick={(e) => e.stopPropagation()}
        className="modal" style={{ width: 460, maxHeight: '85vh', overflow: 'auto' }}>
        {children}
      </div>
    </div>
  );
}

/** Connected: opens when the user clicks "Label speakers" on the
 *  needs-labels toast (via labelModalStore), never automatically off the
 *  phase. Hosts the full SpeakerLabeler. The footer button reflects review
 *  progress: while any speaker still needs a name it reads "Finish later"
 *  (dismiss for now); once every speaker is labeled it turns into an amber
 *  "Save" (acknowledge + close — labels are already persisted per-edit). */
export function SpeakerLabelModal({ onChanged }: { onChanged: () => void | Promise<void> }) {
  const modal = useLabelModal();
  const open = modal.open;
  const sessionId = modal.sessionId;
  const title = modal.title;
  // Leaving the labeling (Save, Finish later, or scrim) kicks the summary
  // tail (summary → chapters → compress → finalized) with whatever labels
  // exist. At the label gate no summary exists yet. Re-arms the progress
  // poll first (the session-status toast shows the summarizing stage),
  // clears the needs-labels phase (drops its toast), and closes the modal.
  const close = () => {
    if (sessionId) {
      beginFinalizeWatch(sessionId, title);
      void resumeFinalize(sessionId);
    }
    dismissPhase();
    closeLabelModal();
  };

  // Tracks the session's speakers; the footer button switches to "Save"
  // once everything is labeled. Mirrors SpeakerLabeler's own data source.
  const [speakers, setSpeakers] = useState<SessionSpeaker[] | null>(null);
  const reload = useCallback(() => {
    if (!sessionId) return;
    tauri.listSessionSpeakers(sessionId).then(setSpeakers).catch(() => setSpeakers([]));
  }, [sessionId]);
  useEffect(() => { setSpeakers(null); reload(); }, [reload]);
  const [recordingKind, setRecordingKind] = useState<RecordingKind | null>(null);
  useEffect(() => {
    setRecordingKind(null);
    if (!sessionId) return;
    tauri.sessionMetaGet(sessionId).then((m) => setRecordingKind(m.recording_kind ?? null)).catch(() => {});
  }, [sessionId]);
  useEffect(() => {
    if (!sessionId) return;
    return subscribeToLibrary((ev) => {
      if (ev.session_id === sessionId || ev.session_id === '') reload();
    });
  }, [sessionId, reload]);

  const allReviewed = !!speakers && speakers.length > 0 && speakers.every((s) => !speakerNeedsReview(s));

  return (
    <SpeakerLabelModalView open={open} onClose={close}>
      {open && sessionId && (
        <>
          <div className="modal__title">Label speakers</div>
          {/* No `clusters` filter: the modal IS the full labeler (same as the
              Participants tab) so it always reflects current diarization —
              including voices grouped by a Diarize click while the modal is
              open. Filtering to the gate's initial cluster ids would make it
              say "no participants matched" even after diarize found speakers. */}
          <SpeakerLabeler
            sessionId={sessionId}
            diarizationUnavailable={false}
            recordingKind={recordingKind}
            inviteAttendees={[]}
            onChanged={onChanged}
          />
          <div style={{ display: 'flex', justifyContent: 'flex-end', marginTop: 12 }}>
            {allReviewed ? (
              <button data-testid="sstat-save" className="btn btn--primary" onClick={close}>Save</button>
            ) : (
              <button data-testid="sstat-later" className="btn" onClick={close}>Finish later</button>
            )}
          </div>
        </>
      )}
    </SpeakerLabelModalView>
  );
}
