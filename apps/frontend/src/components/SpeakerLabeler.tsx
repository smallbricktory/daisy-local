import { useCallback, useEffect, useRef, useState } from 'react';
import {
  tauri,
  errStr,
  type Attendee,
  type DiarizeTrack,
  type SessionSpeaker,
  type VoiceprintView,
} from '../tauri';
import { confirm } from '../lib/confirm';
import { runDiarize, isDiarizing } from '../lib/diarizeBus';
import { subscribeToLibrary } from '../lib/libraryEvents';

// Sentinel value for the pinned "Me" option in the voice picker (not a real
// voiceprint id). Selecting it just names the speaker "Me".
const ME_OPTION = '__me__';

export const STRICT_CONFIDENCE = 0.70;

export function speakerNeedsReview(sp: SessionSpeaker): boolean {
  // Unknown until manually labeled or auto-matched above the strict band.
  if (!sp.is_user_labeled) return true;
  if (sp.match_confidence != null && sp.match_confidence < STRICT_CONFIDENCE) return true;
  return false;
}

export interface SpeakerLabelerProps {
  sessionId: string;
  sessionTitle?: string | null;
  diarizationUnavailable: boolean;
  inviteAttendees: Attendee[];
  clusters?: number[];
  onChanged: () => void | Promise<void>;
}

export function SpeakerLabeler({
  sessionId,
  sessionTitle,
  diarizationUnavailable,
  inviteAttendees,
  clusters,
  onChanged,
}: SpeakerLabelerProps) {
  const [speakers, setSpeakers] = useState<SessionSpeaker[] | null>(null);
  const [editing, setEditing] = useState<SessionSpeaker | null>(null);
  const [diarizing, setDiarizing] = useState(false);
  const [diarizeErr, setDiarizeErr] = useState<string | null>(null);
  // Per-session "# of speakers" for re-diarize. Blank = auto-estimate.
  const [expectedSpeakers, setExpectedSpeakers] = useState('');
  // Which track(s) to diarize. 'auto' (default) lets the backend derive the
  // track from the session (in-person → mic, remote call → system). Explicit
  // values override.
  const [diarizeTrack, setDiarizeTrack] = useState<DiarizeTrack | 'auto'>('auto');
  // "How many speakers" differs per scope: Others counts people besides you
  // (your mic is a separate track); Local mic / Both / Auto cluster the room
  // mic, where your voice is one of the clusters — the count includes you.
  const countLabel = diarizeTrack === 'others' ? 'Other speakers' : 'Speakers';
  const countTitle =
    diarizeTrack === 'others'
      ? 'Number of speakers besides you (your mic is a separate track). Setting this greatly increases accuracy. Leave blank to auto-detect.'
      : diarizeTrack === 'auto'
        ? 'How many voices to find. For a regular call: people besides you. For an in-person recording: everyone, including you. Setting this greatly increases accuracy. Leave blank to auto-detect.'
        : 'Total number of voices on the selected audio — include yourself if you spoke on it. Setting this greatly increases accuracy. Leave blank to auto-detect.';
  const [detachBusy, setDetachBusy] = useState<number | null>(null);
  const [detachErr, setDetachErr] = useState<string | null>(null);
  const [addOpen, setAddOpen] = useState<{ prefName?: string } | null>(null);

  const reload = useCallback(() => {
    tauri.listSessionSpeakers(sessionId).then(setSpeakers).catch(() => setSpeakers([]));
  }, [sessionId]);

  useEffect(() => { reload(); }, [reload]);

  // Any backend mutation to this session (label, diarize, detach, enroll,
  // remove) emits library:changed → refetch the speaker list. The App-level
  // modal has no parent useSessionData.
  useEffect(() => {
    return subscribeToLibrary((ev) => {
      if (ev.session_id === sessionId || ev.session_id === '') reload();
    });
  }, [sessionId, reload]);

  // Diarize runs via the global bus; the work survives navigating away from
  // this pane. App.tsx surfaces a corner toast while it's in flight; local
  // state updates when it finishes.
  const diarize = useCallback(async () => {
    setDiarizing(true); setDiarizeErr(null); setLastResult(null);
    // "# of speakers" field: a positive integer pins k for this call; blank /
    // "auto" / invalid → the diarizer estimates.
    const parsed = Number(expectedSpeakers.trim());
    const known = expectedSpeakers.trim() && Number.isInteger(parsed) && parsed > 0 ? parsed : null;
    try {
      const r = await runDiarize(
        sessionId,
        sessionTitle ?? null,
        known,
        diarizeTrack === 'auto' ? null : diarizeTrack,
      );
      await onChanged();
      reload();
      if (r.speakers === 0) {
        setDiarizeErr(
          diarizeTrack === 'mic'
            ? 'No voices found on your microphone track. If this was a regular call, set "Voices on" to Others (Attendees) and try again.'
            : diarizeTrack === 'others'
              ? 'No voices found in the system audio (the remote side of a call). For an in-person recording, set "Voices on" to Local mic and try again.'
              : diarizeTrack === 'both'
                ? 'No voices found on either track. The recording may be silent — Daisy needs at least one clear ~1-second utterance.'
                : 'No voices found in this recording. If people were in the room with you, set "Voices on" to Local mic; for a regular call, Others (Attendees).',
        );
      } else {
        setLastResult(`Grouped ${r.speakers} voice${r.speakers === 1 ? '' : 's'} (${r.segments_labeled} segment${r.segments_labeled === 1 ? '' : 's'}).`);
      }
    } catch (e) {
      setDiarizeErr(errStr(e));
    } finally {
      setDiarizing(false);
    }
  }, [sessionId, sessionTitle, onChanged, reload, expectedSpeakers, diarizeTrack]);

  // Pick up diarize-done events from the bus for *this* session — covers the
  // case where someone else (auto-rediarize after retranscribe, the App-level
  // toast etc.) finishes a diarize while ParticipantsTab is mounted.
  useEffect(() => {
    const onDone = (ev: Event) => {
      const detail = (ev as CustomEvent<{ sessionId: string }>).detail;
      if (detail.sessionId !== sessionId) return;
      setDiarizing(false);
      // Data reload comes from library:changed (diarize_session emits it).
    };
    const onStart = (ev: Event) => {
      const detail = (ev as CustomEvent<{ sessionId: string }>).detail;
      if (detail.sessionId !== sessionId) return;
      setDiarizing(true);
    };
    window.addEventListener('daisy:diarize-done', onDone as EventListener);
    window.addEventListener('daisy:diarize-start', onStart as EventListener);
    if (isDiarizing(sessionId)) setDiarizing(true);
    return () => {
      window.removeEventListener('daisy:diarize-done', onDone as EventListener);
      window.removeEventListener('daisy:diarize-start', onStart as EventListener);
    };
  }, [sessionId]);

  const detach = useCallback(async (sp: SessionSpeaker) => {
    setDetachBusy(sp.cluster_id); setDetachErr(null);
    try {
      await tauri.detachSpeakerVoiceprint(sessionId, sp.cluster_id);
      await onChanged();
      reload();
    } catch (e) {
      setDetachErr(errStr(e));
    } finally {
      setDetachBusy(null);
    }
  }, [sessionId, onChanged, reload]);

  const [removeBusy, setRemoveBusy] = useState<number | null>(null);
  const [removeErr, setRemoveErr] = useState<string | null>(null);
  const remove = useCallback(async (sp: SessionSpeaker) => {
    const ok = await confirm({
      title: `Remove ${sp.display_name}?`,
      body: `Their segments stay in the transcript but lose this label.`,
      confirmLabel: 'Remove', danger: true,
    });
    if (!ok) return;
    setRemoveBusy(sp.cluster_id); setRemoveErr(null);
    try {
      await tauri.removeSpeakerCluster(sessionId, sp.cluster_id);
      await onChanged();
      reload();
    } catch (e) {
      setRemoveErr(errStr(e));
    } finally {
      setRemoveBusy(null);
    }
  }, [sessionId, onChanged, reload]);

  // State for re-diarize: while active, the button background animates an
  // indeterminate progress bar. Backend diarize_session_impl emits no
  // progress events; lastResult holds a transient success line.
  const [lastResult, setLastResult] = useState<string | null>(null);
  useEffect(() => {
    if (!lastResult) return;
    const t = window.setTimeout(() => setLastResult(null), 6000);
    return () => window.clearTimeout(t);
  }, [lastResult]);

  if (speakers == null) return null;

  const visible = clusters == null ? speakers : speakers.filter((s) => clusters.includes(s.cluster_id));

  // Render the review-status breakdown (Needs review / Voice Matched / Manually
  // Added) for a subset of speakers. Called once flat, or twice (Room / Remote)
  // when the local end has a group too.
  const renderStatusSections = (subset: SessionSpeaker[]) => {
    const unknown = subset.filter(speakerNeedsReview);
    const voiceMatched = subset.filter((s) => !speakerNeedsReview(s) && !!s.voiceprint_id);
    const manuallyAdded = subset.filter((s) => !speakerNeedsReview(s) && !s.voiceprint_id);
    return (
      <>
        {unknown.length > 0 && (
          <ParticipantSection
            title="Needs review"
            hint="Unlabeled or matched with low confidence. Click to label."
            speakers={unknown} detachBusy={detachBusy} removeBusy={removeBusy}
            onEdit={setEditing} onDetach={detach} onRemove={remove} showDetach={false}
          />
        )}
        {voiceMatched.length > 0 && (
          <ParticipantSection
            title="Voice Matched" hint={null}
            speakers={voiceMatched} detachBusy={detachBusy} removeBusy={removeBusy}
            onEdit={setEditing} onDetach={detach} onRemove={remove} showDetach={true}
          />
        )}
        {manuallyAdded.length > 0 && (
          <ParticipantSection
            title="Manually Added" hint={null}
            speakers={manuallyAdded} detachBusy={detachBusy} removeBusy={removeBusy}
            onEdit={setEditing} onDetach={detach} onRemove={remove} showDetach={false}
          />
        )}
      </>
    );
  };

  // Calendar invitees not yet represented in the session. Excludes the user
  // (role === 'self') and any attendee whose display_name / email matches a
  // labelled speaker. Names are case-insensitive trimmed for the compare.
  const normName = (s?: string | null) => (s ?? '').toLowerCase().trim();
  const known = new Set<string>();
  for (const sp of speakers) {
    if (sp.is_user_labeled) {
      const n = normName(sp.display_name);
      if (n) known.add(n);
    }
    if (sp.email) {
      const e = normName(sp.email);
      if (e) known.add(e);
    }
  }
  const pendingInvitees = inviteAttendees.filter(
    (a) => a.role !== 'self' && a.display_name && !known.has(normName(a.display_name)),
  );

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
      {diarizationUnavailable && (
        <div
          data-testid="diarization-unavailable-banner"
          style={{
            padding: '8px 12px', background: 'var(--amber, #FFF3CD)',
            border: '1px solid var(--frost-deep, #D6C9A8)', borderRadius: 6,
            fontSize: 13, color: 'var(--ink)',
          }}
        >
          Voice grouping unavailable — the on-device voice model is missing.
          Reinstall the app or contact support if this persists.
        </div>
      )}

      {visible.length === 0 ? (
        <div style={{
          padding: '12px 14px', border: '1px dashed var(--frost-deep)',
          borderRadius: 8, fontSize: 13, color: 'var(--iron)',
        }}>
          <div>No participants matched yet. Group voices on-device.</div>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap', marginTop: 10 }}>
            <label
              style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}
              title={countTitle}
            >
              {countLabel}
              <input
                type="number"
                min={1}
                inputMode="numeric"
                placeholder="auto"
                value={expectedSpeakers}
                disabled={diarizing}
                onChange={(e) => setExpectedSpeakers(e.target.value)}
                style={{ width: 64 }}
              />
            </label>
            <label
              style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}
              title="Which audio to scan for voices. Auto matches how the meeting was recorded. Others (Attendees) = the remote/system side of a call. Local mic = in-person/room-mic. Both = a group on both ends."
            >
              Voices on
              <select
                value={diarizeTrack}
                disabled={diarizing}
                onChange={(e) => setDiarizeTrack(e.target.value as DiarizeTrack | 'auto')}
              >
                <option value="auto">Auto (match recording)</option>
                <option value="others">Others (Attendees)</option>
                <option value="mic">Local mic</option>
                <option value="both">Both</option>
              </select>
            </label>
            <button
              className={`btn ${diarizing ? 'btn--working' : ''}`}
              disabled={diarizing}
              onClick={() => void diarize()}
            >
              {diarizing ? 'Diarizing…' : 'Diarize voices'}
            </button>
          </div>
          {diarizeErr && <div className="meta" style={{ color: 'var(--danger)', marginTop: 6 }}>{diarizeErr}</div>}
        </div>
      ) : (
        <>
          {(() => {
            const room = visible.filter((s) => s.side === 'room');
            const remote = visible.filter((s) => s.side !== 'room');
            // No room speakers (solo / remote-only): render flat.
            if (room.length === 0) return renderStatusSections(remote);
            return (
              <>
                <div className="side-group">
                  <div className="side-group__title">Your room</div>
                  {renderStatusSections(room)}
                </div>
                <div className="side-group">
                  <div className="side-group__title">Remote</div>
                  {renderStatusSections(remote)}
                </div>
              </>
            );
          })()}
          {pendingInvitees.length > 0 && (
            <InviteSection
              attendees={pendingInvitees}
              onAdd={(name) => setAddOpen({ prefName: name })}
            />
          )}
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
            <label
              style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}
              title={countTitle}
            >
              {countLabel}
              <input
                type="number"
                min={1}
                inputMode="numeric"
                placeholder="auto"
                value={expectedSpeakers}
                disabled={diarizing}
                onChange={(e) => setExpectedSpeakers(e.target.value)}
                style={{ width: 64 }}
              />
            </label>
            <label
              style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 13 }}
              title="Which audio to diarize. Others (Attendees) = the remote/system track (default). Local mic = an in-person/room-mic recording where the people are on your mic. Both = a group on both ends."
            >
              Voices on
              <select
                value={diarizeTrack}
                disabled={diarizing}
                onChange={(e) => setDiarizeTrack(e.target.value as DiarizeTrack | 'auto')}
              >
                <option value="auto">Auto (match recording)</option>
                <option value="others">Others (Attendees)</option>
                <option value="mic">Local mic</option>
                <option value="both">Both</option>
              </select>
            </label>
            <button
              className={`btn ${diarizing ? 'btn--working' : ''}`}
              disabled={diarizing}
              onClick={() => void diarize()}
              title="Re-cluster the selected track's voices locally (on-device). Resets auto labels; your manual names are re-applied by voiceprint match."
            >
              {diarizing ? 'Diarizing…' : 'Re-diarize'}
            </button>
            <button className="btn" onClick={() => setAddOpen({})}>
              + Add participant
            </button>
            {lastResult && <span className="meta" role="status" style={{ color: 'var(--ok, #2d6a4f)' }}>{lastResult}</span>}
            {diarizeErr && <span className="meta" style={{ color: 'var(--danger)' }}>{diarizeErr}</span>}
            {detachErr && <span className="meta" style={{ color: 'var(--danger)' }}>{detachErr}</span>}
            {removeErr && <span className="meta" style={{ color: 'var(--danger)' }}>{removeErr}</span>}
          </div>
        </>
      )}

      {editing && (
        <SpeakerEditModal
          sessionId={sessionId}
          speaker={editing}
          onClose={() => setEditing(null)}
          onSaved={async () => { setEditing(null); reload(); await onChanged(); }}
        />
      )}
      {addOpen && (
        <AddParticipantModal
          sessionId={sessionId}
          prefName={addOpen.prefName ?? ''}
          onClose={() => setAddOpen(null)}
          onSaved={async () => { setAddOpen(null); reload(); await onChanged(); }}
        />
      )}
    </div>
  );
}

function InviteSection({
  attendees, onAdd,
}: {
  attendees: Attendee[];
  onAdd: (name: string) => void;
}) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      <div style={{
        fontFamily: 'var(--font-mono)', fontSize: 10,
        letterSpacing: '0.14em', textTransform: 'uppercase', color: 'var(--iron)',
      }}>From Invite</div>
      <div className="meta" style={{ fontSize: 12, color: 'var(--iron)' }}>
        Calendar attendees not yet detected. Click + to add as a manual participant.
      </div>
      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
        {attendees.map((a, i) => (
          <div
            key={`${a.display_name}-${i}`}
            style={{
              display: 'flex', alignItems: 'center', gap: 10,
              padding: '6px 10px', border: '1px dashed var(--frost-deep)',
              borderRadius: 8, background: 'transparent',
            }}
          >
            <div style={{ flex: 1, fontSize: 14, color: 'var(--iron)' }}>
              {a.display_name}
            </div>
            <button
              className="btn"
              onClick={() => onAdd(a.display_name)}
              title="Add this invitee as a manual participant"
            >
              + Add
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}

function AddParticipantModal({
  sessionId, prefName, onClose, onSaved,
}: {
  sessionId: string;
  prefName: string;
  onClose: () => void;
  onSaved: () => void | Promise<void>;
}) {
  const [name, setName] = useState(prefName);
  const [email, setEmail] = useState('');
  const [voiceprintId, setVoiceprintId] = useState('');
  const [voiceprints, setVoiceprints] = useState<VoiceprintView[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    tauri.listVoiceprints()
      .then((v) => setVoiceprints(
        [...v].sort((a, b) => a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' })),
      ))
      .catch(() => setVoiceprints([]));
  }, []);

  // Picking a voiceprint pre-fills the form with that identity's name/email.
  function selectVoiceprint(id: string) {
    setVoiceprintId(id);
    if (!id) return;
    const vp = voiceprints?.find((v) => v.id === id);
    if (vp) {
      if (!name.trim()) setName(vp.display_name);
      if (!email.trim() && vp.email) setEmail(vp.email);
    }
  }

  async function save() {
    if (!name.trim()) { setErr('Name is required.'); return; }
    setBusy(true); setErr(null);
    try {
      await tauri.addSessionSpeaker(
        sessionId,
        name.trim(),
        email.trim() ? email.trim() : null,
        voiceprintId || null,
      );
      await onSaved();
    } catch (e) {
      setErr(errStr(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div role="dialog" aria-modal="true" style={{
      position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.35)',
      display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 10,
    }}>
      <div style={{
        background: 'var(--cream-pure)', borderRadius: 8, padding: 20, width: 360,
        boxShadow: '0 6px 30px rgba(0,0,0,0.18)',
      }}>
        <h3 className="h3" style={{ marginTop: 0, marginBottom: 12 }}>Add participant</h3>

        <label style={{ display: 'block', fontSize: 12, color: 'var(--iron)', marginBottom: 4 }}>
          Match to existing voice (optional)
        </label>
        <select
          value={voiceprintId}
          disabled={busy}
          onChange={(e) => selectVoiceprint(e.target.value)}
          style={{ display: 'block', width: '100%', marginBottom: 10 }}
        >
          <option value="">— none —</option>
          {(voiceprints ?? []).map((v) => (
            <option key={v.id} value={v.id}>
              {v.display_name}{v.email ? ` · ${v.email}` : ''}
            </option>
          ))}
        </select>

        <label style={{ display: 'block', fontSize: 12, color: 'var(--iron)', marginBottom: 4 }}>
          Name
        </label>
        <input
          type="text" value={name} disabled={busy}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Jane Doe"
          style={{ display: 'block', width: '100%', marginBottom: 10 }}
        />

        <label style={{ display: 'block', fontSize: 12, color: 'var(--iron)', marginBottom: 4 }}>
          Email (optional)
        </label>
        <input
          type="email" value={email} disabled={busy}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="jane@example.com"
          style={{ display: 'block', width: '100%', marginBottom: 14 }}
        />

        {err && <p className="meta" style={{ color: 'var(--danger)', fontSize: 12 }}>{err}</p>}

        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button className="btn" disabled={busy} onClick={onClose}>Cancel</button>
          <button className="btn btn--primary" disabled={busy || !name.trim()} onClick={() => void save()}>
            {busy ? 'Adding…' : 'Add'}
          </button>
        </div>
      </div>
    </div>
  );
}

function ParticipantSection({
  title, hint, speakers, detachBusy, removeBusy, onEdit, onDetach, onRemove, showDetach,
}: {
  title: string;
  hint: string | null;
  speakers: SessionSpeaker[];
  detachBusy: number | null;
  removeBusy: number | null;
  onEdit: (sp: SessionSpeaker) => void;
  onDetach: (sp: SessionSpeaker) => void;
  onRemove: (sp: SessionSpeaker) => void;
  showDetach: boolean;
}) {
  return (
    <div className="participant-section">
      <div className="participant-section__title">{title}</div>
      {hint && <div className="participant-section__hint">{hint}</div>}
      <div className="participant-list">
        {speakers.map((sp) => (
          <ParticipantRow
            key={sp.cluster_id}
            sp={sp}
            onEdit={onEdit}
            onDetach={onDetach}
            onRemove={onRemove}
            detachBusy={detachBusy === sp.cluster_id}
            removeBusy={removeBusy === sp.cluster_id}
            showDetach={showDetach}
          />
        ))}
      </div>
    </div>
  );
}

function ParticipantRow({
  sp, onEdit, onDetach, onRemove, detachBusy, removeBusy, showDetach,
}: {
  sp: SessionSpeaker;
  onEdit: (sp: SessionSpeaker) => void;
  onDetach: (sp: SessionSpeaker) => void;
  onRemove: (sp: SessionSpeaker) => void;
  detachBusy: boolean;
  removeBusy: boolean;
  showDetach: boolean;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const onDoc = (e: MouseEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenuOpen(false);
    };
    const onEsc = (e: KeyboardEvent) => { if (e.key === 'Escape') setMenuOpen(false); };
    window.addEventListener('mousedown', onDoc);
    window.addEventListener('keydown', onEsc);
    return () => {
      window.removeEventListener('mousedown', onDoc);
      window.removeEventListener('keydown', onEsc);
    };
  }, [menuOpen]);

  const confPct = sp.match_confidence != null
    ? `${Math.round(Math.max(0, Math.min(1, sp.match_confidence)) * 100)}%`
    : null;
  const showWrongMatch = showDetach && !!sp.voiceprint_id;

  return (
    <div className="participant-row">
      <button
        className="participant-row__main"
        onClick={() => onEdit(sp)}
        title={sp.sample_text ? `Sample: "${sp.sample_text}"` : undefined}
      >
        <div className="participant-row__name" data-labeled={sp.is_user_labeled ? '1' : '0'}>
          {sp.display_name}
        </div>
        <div className="participant-row__meta">
          {[sp.email, confPct ? `match ${confPct}` : null, sp.voiceprint_id ? 'linked' : null]
            .filter(Boolean)
            .join(' · ')}
        </div>
      </button>
      <div className="participant-row__menu" ref={menuRef}>
        <button
          type="button"
          className="participant-row__kebab"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          aria-label="Row actions"
          disabled={detachBusy || removeBusy}
          onClick={() => setMenuOpen((v) => !v)}
        >
          {detachBusy || removeBusy ? '…' : '⋯'}
        </button>
        {menuOpen && (
          <div role="menu" className="participant-row__dropdown">
            {showWrongMatch && (
              <button
                className="action-menu__item"
                role="menuitem"
                onClick={() => { setMenuOpen(false); onDetach(sp); }}
                disabled={detachBusy}
              >
                Wrong match
              </button>
            )}
            <button
              className="action-menu__item participant-row__dropdown-danger"
              role="menuitem"
              onClick={() => { setMenuOpen(false); onRemove(sp); }}
              disabled={removeBusy}
            >
              Remove
            </button>
          </div>
        )}
      </div>
    </div>
  );
}


function SpeakerEditModal({
  sessionId, speaker, onClose, onSaved,
}: {
  sessionId: string;
  speaker: SessionSpeaker;
  onClose: () => void;
  onSaved: () => void | Promise<void>;
}) {
  const [name, setName] = useState(speaker.is_user_labeled ? speaker.display_name : '');
  const [email, setEmail] = useState(speaker.email ?? '');
  const [enroll, setEnroll] = useState(true);
  const [backfill, setBackfill] = useState(false);
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // Existing voices for the "match to existing voice" selector. Picking one
  // copies its display_name/email into the form; submitting then routes
  // through enrollVoiceprintFromSpeaker, which appends to the matched
  // identity's gallery (the backend dedupes by email-or-name).
  const [voiceprints, setVoiceprints] = useState<VoiceprintView[] | null>(null);
  const [selectedVoiceprintId, setSelectedVoiceprintId] = useState<string>('');
  useEffect(() => {
    tauri.listVoiceprints()
      .then((v) => setVoiceprints(
        [...v].sort((a, b) => a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' })),
      ))
      .catch(() => setVoiceprints([]));
  }, []);
  function selectExistingVoiceprint(id: string) {
    setSelectedVoiceprintId(id);
    if (!id) return;
    if (id === ME_OPTION) {
      // Pinned "Me". Backend dedupes by display name; the first enroll
      // creates the "Me" identity and later ones append to it.
      setName('Me');
      setEmail('');
      setEnroll(true);
      return;
    }
    const vp = voiceprints?.find((v) => v.id === id);
    if (vp) {
      setName(vp.display_name);
      setEmail(vp.email ?? '');
      setEnroll(true);
    }
  }

  // Lazily-loaded ~5s WAV of the cluster's audio. Built on first Play
  // click then cached on the Blob URL for replay. URL is revoked when
  // the modal closes / a different cluster is opened.
  const [sampleUrl, setSampleUrl] = useState<string | null>(null);
  const [sampleBusy, setSampleBusy] = useState(false);
  const [sampleErr, setSampleErr] = useState<string | null>(null);
  useEffect(() => () => {
    if (sampleUrl) URL.revokeObjectURL(sampleUrl);
  }, [sampleUrl]);

  // Sample snippet: the transcript of exactly the segments the Play clip
  // uses. Falls back to the list's sample_text until it loads.
  // speaker.sample_text picks the first transcribed segment; the clip skips
  // silence / spans tracks.
  const [matchedSample, setMatchedSample] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    setMatchedSample(null);
    tauri.sessionSpeakerSampleText(sessionId, speaker.cluster_id)
      .then((t) => { if (alive && t.trim()) setMatchedSample(t); })
      .catch(() => {});
    return () => { alive = false; };
  }, [sessionId, speaker.cluster_id]);
  const shownSample = matchedSample ?? speaker.sample_text;

  async function loadAndPlay() {
    if (sampleUrl) {
      const a = document.getElementById(`speaker-sample-${speaker.cluster_id}`) as HTMLAudioElement | null;
      if (a) { a.currentTime = 0; void a.play().catch(() => {}); }
      return;
    }
    setSampleBusy(true); setSampleErr(null);
    try {
      const bytes = await tauri.sessionSpeakerSampleAudioBytes(sessionId, speaker.cluster_id);
      const blob = new Blob([bytes], { type: 'audio/wav' });
      setSampleUrl(URL.createObjectURL(blob));
    } catch (e) {
      setSampleErr(errStr(e));
    } finally {
      setSampleBusy(false);
    }
  }

  async function save() {
    setBusy(true); setErr(null);
    try {
      if (enroll) {
        // Enrolls a voiceprint; future sessions auto-recognize this person.
        // Failure falls through to label-only; the per-session label is kept.
        try {
          await tauri.enrollVoiceprintFromSpeaker(
            sessionId, speaker.cluster_id, name, email.trim() || null,
          );
          // Optional: back-fill past meetings where this person spoke but was
          // never labeled. Slow (reloads the model + scans every session);
          // opt-in, runs after the enrollment is saved.
          if (backfill) {
            setScanning(true);
            try {
              await tauri.rematchAllSessions();
            } catch (e) {
              setErr(`Voiceprint saved. Past-meeting scan failed: ${errStr(e)}`);
            } finally {
              setScanning(false);
            }
          }
        } catch (e) {
          await tauri.setSessionSpeakerLabel(
            sessionId, speaker.cluster_id, name, email.trim() || null,
          );
          setErr(`Saved name. Voiceprint enrollment skipped: ${errStr(e)}`);
        }
      } else {
        await tauri.setSessionSpeakerLabel(
          sessionId, speaker.cluster_id, name, email.trim() || null,
        );
      }
      await onSaved();
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }
  async function clearLabel() {
    setBusy(true); setErr(null);
    try {
      await tauri.setSessionSpeakerLabel(sessionId, speaker.cluster_id, '', null);
      await onSaved();
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }
  async function removeCluster() {
    const ok = await confirm({
      title: 'Remove this voice cluster?',
      body: 'The segments revert to no-speaker. You can re-cluster later via Re-diarize.',
      confirmLabel: 'Remove', danger: true,
    });
    if (!ok) return;
    setBusy(true); setErr(null);
    try {
      await tauri.removeSpeakerCluster(sessionId, speaker.cluster_id);
      await onSaved();
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }
  return (
    <div className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 460 }}>
        <h2 className="h2" style={{ marginTop: 0 }}>Label speaker</h2>
        {shownSample && (
          <p className="meta" style={{ fontSize: 12, marginBottom: 6 }}>
            Sample: <em>“{shownSample}”</em>
          </p>
        )}
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 12 }}>
          <button
            className="btn"
            onClick={() => void loadAndPlay()}
            disabled={busy || sampleBusy}
            title="Play a ~5s clip of this speaker so you can ID the voice before naming them."
          >
            {sampleBusy ? 'Loading…' : (sampleUrl ? '▶ Play again' : '▶ Play sample audio')}
          </button>
          {sampleUrl && (
            <audio
              id={`speaker-sample-${speaker.cluster_id}`}
              src={sampleUrl}
              autoPlay
              controls
              style={{ flex: 1, height: 32 }}
            />
          )}
        </div>
        {sampleErr && <p className="meta" style={{ color: 'var(--danger)', fontSize: 12, marginBottom: 8 }}>{sampleErr}</p>}
        {voiceprints && (
          <>
            <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>
              Match to existing voice
            </label>
            <select
              value={selectedVoiceprintId}
              disabled={busy}
              onChange={(e) => selectExistingVoiceprint(e.target.value)}
              style={{ width: '100%', marginBottom: 12 }}
            >
              <option value="">— New voice —</option>
              {/* "Me" pinned at the top. The backend dedupes by name; later
                  sessions append to the same "Me" identity (voiceprint match
                  auto-fills it). */}
              <option value={ME_OPTION}>Me</option>
              {voiceprints
                .filter((v) => v.display_name.trim().toLowerCase() !== 'me')
                .map((v) => (
                  <option key={v.id} value={v.id}>
                    {v.display_name}{v.email ? ` · ${v.email}` : ''}
                  </option>
                ))}
            </select>
          </>
        )}
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Name</label>
        <input
          type="text" autoFocus value={name} disabled={busy}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Sam Perkins"
          style={{ width: '100%', marginBottom: 12 }}
        />
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Email (optional)</label>
        <input
          type="email" value={email} disabled={busy}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="sam@example.com"
          style={{ width: '100%', marginBottom: 12 }}
        />
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13, marginBottom: 12 }}>
          <input
            type="checkbox" checked={enroll} disabled={busy}
            onChange={(e) => setEnroll(e.target.checked)}
          />
          <span>
            Save voiceprint for future meetings
            <br />
            <span className="meta" style={{ fontSize: 11 }}>
              Stores an encrypted voice embedding in the vault so Daisy can recognize this person automatically.
              You can delete it any time in Settings → Voiceprints.
            </span>
          </span>
        </label>
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 13, marginBottom: 12, opacity: enroll ? 1 : 0.5 }}>
          <input
            type="checkbox" checked={backfill} disabled={busy || !enroll}
            onChange={(e) => setBackfill(e.target.checked)}
          />
          <span>
            Also check past meetings for this person
            <br />
            <span className="meta" style={{ fontSize: 11 }}>
              Scans every past recording for unlabeled speakers that match this voiceprint and labels them.
              Diarization isn't perfect, so mistakes are always possible — review the results.
              Can take a few minutes if you have many meetings.
            </span>
          </span>
        </label>
        {scanning && <p className="meta">Scanning past meetings… this can take a few minutes.</p>}
        {err && <p className="meta" style={{ color: 'var(--danger)' }}>{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', flexWrap: 'wrap' }}>
          <button
            className="btn"
            onClick={() => void removeCluster()}
            disabled={busy}
            title="Drop this cluster from the session — segments revert to no-speaker. Re-diarize re-clusters."
            style={{ color: 'var(--danger)' }}
          >
            Remove cluster
          </button>
          {speaker.is_user_labeled && (
            <button className="btn" onClick={() => void clearLabel()} disabled={busy}>
              Clear label
            </button>
          )}
          <button className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button className="btn btn--primary" onClick={() => void save()} disabled={busy || !name.trim()}>
            {scanning ? 'Scanning…' : busy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  );
}
