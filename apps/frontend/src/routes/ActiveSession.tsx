import { memo, useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { tauri, errStr, type AudioSourceInfo, type EventSeed, type Settings, type Tag } from '../tauri';
import { ResizableSplit } from '../components/ResizableSplit';
import { CallChat } from '../components/CallChat';
import { TagChip } from '../components/tags/TagChip';
import { TagCombobox } from '../components/tags/TagCombobox';
import { TagPromptModal } from '../components/tags/TagPromptModal';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { MicLevel } from '../components/MicLevel';
import { AudioVisualizer } from '../components/AudioVisualizer';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  appendPauseMarker,
  getLiveTranscriptState,
  isPauseMarker,
  resetLiveTranscript,
  subscribeLiveTranscript,
  type LiveTurn,
} from '../liveTranscript';
import { useVisibleInterval } from '../lib/useVisibleInterval';

interface Props {
  /**
   * Called when the user finishes a session and chooses to process it. The
   * finalize+summarize cascade then runs in the background (owned by App);
   * this screen returns control immediately.
   */
  onProcessingStarted: (sessionId: string, title: string) => void;
  /**
   * If present, the user navigated here by clicking a Calendar event.
   * Pre-fills the title + auto-tag + manifest.calendar back-reference
   * when the recording actually starts.
   */
  eventSeed?: EventSeed;
  /** Called after the user discards an in-progress recording (or backs out of
   *  the pre-recording picker) — App navigates back to the Library. */
  onDiscarded: () => void;
}

// Screen states: 'picking' = mic picker (pre-recording), 'recording' = live,
// 'starting'/'stopping' = transient backend calls, 'stopped' = paused & resumable.
type ScreenState = 'picking' | 'starting' | 'recording' | 'stopping' | 'stopped';

const NOTES_DEBOUNCE_MS = 500;

// Default new-recording title, e.g. "11:24AM - Ad hoc".
function defaultRecordingTitle(d: Date = new Date()): string {
  let h = d.getHours();
  const m = d.getMinutes();
  const ampm = h < 12 ? 'AM' : 'PM';
  h = h % 12;
  if (h === 0) h = 12;
  return `${h}:${String(m).padStart(2, '0')}${ampm} - Ad hoc`;
}

function fmtElapsed(seconds: number): string {
  const safe = Math.max(0, seconds);
  const h = Math.floor(safe / 3600);
  const m = Math.floor((safe % 3600) / 60);
  const s = safe % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}


// Memoized. The liveTranscript store preserves object identity for unchanged
// finals across updates (push + polish-merge); the default shallow comparison
// skips re-rendering unchanged turns.
const LiveLine = memo(function LiveLine({ turn, tail }: { turn: LiveTurn; tail?: string }) {
  const cls = turn.track === 'mic' ? 'turn me' : 'turn them';
  return (
    <div className={cls} style={{ opacity: turn.isInterim ? 0.65 : 1 }}>
      {turn.track !== 'mic' && <span className="who">Them</span>}
      <span className="turn-text" style={turn.isInterim ? { fontStyle: 'italic' } : undefined}>
        {turn.text}
        {turn.isInterim && ' …'}
        {tail != null && <em style={{ opacity: 0.65 }}> {tail} …</em>}
      </span>
    </div>
  );
});

export function ActiveSession({ onProcessingStarted, eventSeed, onDiscarded }: Props) {
  const [screen, setScreen] = useState<ScreenState>('picking');
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [liveModeLabel, setLiveModeLabel] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Elapsed clock — derived from startedAt while recording; frozen when stopped.
  const [elapsed, setElapsed] = useState(0);
  const [elapsedAtPause, setElapsedAtPause] = useState<number | null>(null);
  // Local mic mute during recording (record system audio only, e.g. YouTube).
  const [micMuted, setMicMuted] = useState(false);
  // In-call mic level, driven by the backend `recording:mic-level` event (the
  // recording's own mic stream — mute-aware, no second capture engine).
  const [recMicLevel, setRecMicLevel] = useState(0);

  // Mic picker state — loaded on mount, used session-only (not written back).
  const [micSources, setMicSources] = useState<AudioSourceInfo[]>([]);
  const [selectedMicId, setSelectedMicId] = useState<number | null>(null);
  // Defaults to ON ("just me on this end"). Not persisted; opted into fresh
  // each recording. Unchecking diarizes the mic (room) track too.
  const [singleLocalSpeaker, setSingleLocalSpeaker] = useState(true);
  const [loadedSettings, setLoadedSettings] = useState<Settings | null>(null);

  // Notebook metadata (during recording).
  const [title, setTitle] = useState('');
  const [lastSavedTitle, setLastSavedTitle] = useState('');
  // Title typed on the pre-recording screen; defaults to the current time.
  const [startTitle, setStartTitle] = useState(() => eventSeed?.title ?? defaultRecordingTitle());
  // Tags picked on the pre-record screen. Written to the manifest at start AND
  // used to prime live captions (names/jargon) from the first chunk.
  const [preRecordTags, setPreRecordTags] = useState<Tag[]>([]);
  const [notes, setNotes] = useState('');
  const [attachedIds, setAttachedIds] = useState<string[]>([]);
  const [tagsById, setTagsById] = useState<Record<string, Tag>>({});
  const [promptModalTag, setPromptModalTag] = useState<Tag | null>(null);
  // Live-captions working-view note; per-mount (reappears on the next recording).
  const [liveNoteDismissed, setLiveNoteDismissed] = useState(false);

  // Live transcript — held in a module-level store; the buffer survives page
  // navigation. The listener is attached lazily on first subscribe and keeps
  // running for the lifetime of the app process.
  const live = useSyncExternalStore(subscribeLiveTranscript, getLiveTranscriptState);
  const { finals, interimMic, interimSystem, liveError } = live;
  const transcriptEndRef = useRef<HTMLDivElement>(null);

  const notesTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const notesRef = useRef('');
  notesRef.current = notes;

  // ---- elapsed clock ----------------------------------------------------
  // One-shot sync on screen/start change, then a visibility-gated ticker.
  // If the user minimizes Daisy mid-recording, the clock stops re-rendering
  // until they come back — wall time still wins on the next tick.
  useEffect(() => {
    if (screen !== 'recording' || startedAt === null) return;
    setElapsed(Math.max(0, Math.floor(Date.now() / 1000) - startedAt));
  }, [screen, startedAt]);
  useVisibleInterval(() => {
    if (startedAt === null) return;
    setElapsed(Math.max(0, Math.floor(Date.now() / 1000) - startedAt));
  }, 1000, screen === 'recording' && startedAt !== null);

  // ---- in-call mic level -----------------------------------------------
  // The backend emits the recording's own mic peak (`recording:mic-level`);
  // the bar reflects the actual recorded signal (mute → 0).
  useEffect(() => {
    if (screen !== 'recording') { setRecMicLevel(0); return; }
    let cancelled = false;
    let un: UnlistenFn | null = null;
    void (async () => {
      un = await listen<{ peak: number }>('recording:mic-level', (e) => {
        if (cancelled) return;
        // Backend sends a raw normalized peak; a sqrt curve shapes the bar.
        const p = Math.max(0, e.payload?.peak ?? 0);
        setRecMicLevel(Math.min(1, Math.sqrt(p) * 1.6));
      });
      if (cancelled && un) { un(); un = null; }
    })();
    return () => { cancelled = true; if (un) un(); };
  }, [screen]);

  // ---- mount: load mics + settings -------------------------------------
  useEffect(() => {
    Promise.all([tauri.listAudioSources(), tauri.readSettings()])
      .then(([sources, settings]) => {
        const mics = sources.filter((s) => s.kind === 'mic');
        setMicSources(mics);
        setLoadedSettings(settings);
        const defaultId = settings.default_mic_source_id;
        if (defaultId !== null && mics.some((m) => m.id === defaultId)) {
          setSelectedMicId(defaultId);
        } else if (mics.length > 0) {
          setSelectedMicId(mics[0].id);
        }
      })
      .catch(() => {
        /* non-fatal: mic picker shows empty */
      });
  }, []);

  // Re-enumerates mics when the user opens the in-call switcher; a device
  // connected mid-call (e.g. AirPods) shows up on open.
  const refreshMicSources = useCallback(() => {
    tauri
      .listAudioSources()
      .then((sources) => setMicSources(sources.filter((s) => s.kind === 'mic')))
      .catch(() => {});
  }, []);

  // ---- mount: load tag dictionary --------------------------------------
  useEffect(() => {
    tauri
      .listTags()
      .then((tags) => {
        const map: Record<string, Tag> = {};
        for (const t of tags) map[t.id] = t;
        setTagsById(map);
      })
      .catch(() => {
        /* non-fatal */
      });
  }, []);

  // ---- mount: pick up an already-active recording ----------------------
  useEffect(() => {
    tauri
      .recordingSnapshot()
      .then(async (snap) => {
        if (!snap) return;
        if (snap.state !== 'recording' && snap.state !== 'paused') return;
        setSessionId(snap.session_id);
        // The live store is claimed for this session id when it is empty or
        // tagged with a different session (e.g. app just started). An
        // existing matching buffer is kept.
        if (getLiveTranscriptState().sessionId !== snap.session_id) {
          resetLiveTranscript(snap.session_id);
        }
        setStartedAt(snap.started_at_unix_seconds);
        setLiveModeLabel(snap.live_mode_label);
        const now = Math.floor(Date.now() / 1000);
        const e = Math.max(0, now - snap.started_at_unix_seconds);
        setElapsed(e);
        if (snap.state === 'paused') {
          setElapsedAtPause(e);
          setScreen('stopped');
        } else {
          setScreen('recording');
        }
        // Hydrate the notebook fields from disk.
        try {
          const meta = await tauri.sessionMetaGet(snap.session_id);
          setTitle(meta.title ?? '');
          setLastSavedTitle(meta.title ?? '');
          setAttachedIds(meta.tag_ids);
        } catch {
          /* leave blank */
        }
        try {
          const md = await tauri.sessionNotesLoad(snap.session_id);
          setNotes(md);
        } catch {
          /* leave blank */
        }
      })
      .catch(() => {
        /* no active recording */
      });
  }, []);

  // The live transcript listener lives in the module store
  // (`liveTranscript.ts`) — no per-mount wiring here.

  // ---- auto-scroll near bottom -----------------------------------------
  useEffect(() => {
    const el = transcriptEndRef.current?.parentElement;
    if (!el) return;
    const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceFromBottom < 100) {
      transcriptEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [finals, interimMic, interimSystem]);

  // ---- notes debounce cleanup ------------------------------------------
  useEffect(
    () => () => {
      if (notesTimer.current) clearTimeout(notesTimer.current);
    },
    [],
  );

  function flushNotes(): void {
    const sid = sessionId;
    if (!sid) return;
    if (notesTimer.current) {
      clearTimeout(notesTimer.current);
      notesTimer.current = null;
    }
    tauri.sessionNotesSave(sid, notesRef.current).catch(() => {
      /* best effort */
    });
  }

  function onNotesChange(value: string): void {
    setNotes(value);
    const sid = sessionId;
    if (!sid) return;
    if (notesTimer.current) clearTimeout(notesTimer.current);
    notesTimer.current = setTimeout(() => {
      notesTimer.current = null;
      tauri.sessionNotesSave(sid, value).catch(() => {
        /* best effort */
      });
    }, NOTES_DEBOUNCE_MS);
  }

  function onTitleBlur(): void {
    const sid = sessionId;
    if (!sid) return;
    if (title === lastSavedTitle) return;
    const next = title.trim() || null;
    tauri.sessionMetaUpdate({ session_id: sid, title: next }).catch(() => {
      /* best effort */
    });
    setLastSavedTitle(title);
  }

  function addTag(t: Tag): void {
    const sid = sessionId;
    if (!sid || attachedIds.includes(t.id)) return;
    const next = [...attachedIds, t.id];
    setAttachedIds(next);
    setTagsById((m) => ({ ...m, [t.id]: t }));
    tauri.sessionAssignTags(sid, next).catch(() => {
      /* best effort */
    });
  }

  function removeTag(tagId: string): void {
    const sid = sessionId;
    if (!sid) return;
    const next = attachedIds.filter((id) => id !== tagId);
    setAttachedIds(next);
    tauri.sessionAssignTags(sid, next).catch(() => {
      /* best effort */
    });
  }

  // ---- recording lifecycle ---------------------------------------------
  async function start(): Promise<void> {
    setError(null);
    // Wipe any prior session's transcript history before starting a new one.
    resetLiveTranscript(null);
    setLiveModeLabel(null);
    setElapsedAtPause(null);
    if (!selectedMicId || selectedMicId <= 0) {
      setError(
        micSources.length === 0
          ? 'No microphones detected. Connect a microphone and try again.'
          : 'No microphone selected. Pick one from the dropdown above.',
      );
      return;
    }
    setScreen('starting');
    const initialTitle = startTitle.trim();
    // Tags chosen on the pre-record screen (+ any calendar auto-tag), deduped.
    // Sent to the backend AND seeded into the in-call notebook state below.
    const startTagIds = (() => {
      const ids = preRecordTags.map((t) => t.id);
      if (eventSeed?.tag_id && !ids.includes(eventSeed.tag_id)) ids.push(eventSeed.tag_id);
      return ids;
    })();
    try {
      const snapshot = await tauri.startRecording({
        mic_source_id: selectedMicId,
        system_source_id: null,
        single_local_speaker: singleLocalSpeaker,
        session_id: null,
        title: initialTitle || null,
        tag_ids: startTagIds.length ? startTagIds : undefined,
        calendar_link: eventSeed?.calendar_link ?? null,
        // Calendar attendees become "other" participants (the local user is
        // the "self" track). Fall back to email when a name is missing; drop
        // entries with neither.
        attendees: eventSeed?.attendees
          ?.map((a) => ({
            display_name: (a.display_name ?? a.email ?? '').trim(),
            role: 'other' as const,
          }))
          .filter((a) => a.display_name !== ''),
      });
      setSessionId(snapshot.session_id);
      // Tags the live-transcript store with this session id; a later mount
      // compares it against the recording it sees.
      resetLiveTranscript(snapshot.session_id);
      setStartedAt(snapshot.started_at_unix_seconds);
      setLiveModeLabel(snapshot.live_mode_label);
      setElapsed(0);
      setTitle(initialTitle);
      setLastSavedTitle(initialTitle);
      setNotes('');
      // Carries the pre-record tags into the in-call notebook (already
      // persisted via tag_ids above).
      if (preRecordTags.length) {
        setTagsById((m) => {
          const next = { ...m };
          for (const t of preRecordTags) next[t.id] = t;
          return next;
        });
      }
      setAttachedIds(startTagIds);
      setScreen('recording');
    } catch (e: unknown) {
      setError(errStr(e));
      setScreen('picking');
    }
  }

  // The "Pause recording" button. `pause_recording` closes the chunk WAV +
  // active segment and keeps the session resumable. `stop_recording`, which
  // makes the session un-resumable, is called later by the background
  // finalize flow.
  async function onStop(): Promise<void> {
    flushNotes();
    setError(null);
    const frozen = elapsed;
    setScreen('stopping');
    try {
      await tauri.pauseRecording();
      setElapsedAtPause(frozen);
      appendPauseMarker(fmtElapsed(frozen));
      setScreen('stopped');
    } catch (e: unknown) {
      setError(errStr(e));
      setScreen('recording');
    }
  }

  async function onResume(): Promise<void> {
    setError(null);
    setScreen('starting');
    try {
      await tauri.resumeRecording();
      setElapsedAtPause(null);
      setScreen('recording');
    } catch (e: unknown) {
      setError(errStr(e));
      setScreen('stopped');
    }
  }

  // Discard the in-progress session entirely: stop the recorder and delete
  // its directory. Irreversible — confirmed via the shared ConfirmDialog.
  async function doDiscard(): Promise<void> {
    await tauri.cancelRecording();
    resetLiveTranscript(null);
    onDiscarded();
  }

  // Hands the session off to the background processing flow in App. It
  // releases the recording slot and runs the
  // finalize+transcribe+dedup+summarize cascade.
  async function onProcess(): Promise<void> {
    const sid = sessionId;
    if (!sid) return;
    flushNotes();

    // The session moves to the background cascade. The live buffer is
    // cleared; a fresh recording starts blank.
    resetLiveTranscript(null);
    onProcessingStarted(sid, title.trim() || sid);
  }

  // ---- mic picker (pre-recording) --------------------------------------
  if (screen === 'picking' || screen === 'starting') {
    const busy = screen === 'starting';
    return (
      <div
        className="recorder"
        style={{
          display: 'flex', flexDirection: 'column', alignItems: 'center',
          justifyContent: 'center', textAlign: 'center', minHeight: '70vh', gap: 4,
        }}
      >
        <h1 className="h1" style={{ marginBottom: 4 }}>New recording</h1>
        <p className="meta" style={{ marginBottom: 20 }}>Set a title and mic, then start.</p>

        <div style={{ marginBottom: 14, textAlign: 'left' }}>
          <label htmlFor="rec-title" style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Title</label>
          <input
            id="rec-title"
            type="text"
            value={startTitle}
            disabled={busy}
            onChange={(e) => setStartTitle(e.target.value)}
            placeholder="Meeting title"
            style={{ display: 'block', width: 340 }}
          />
        </div>

        <div style={{ marginBottom: 22, textAlign: 'left' }}>
          <label htmlFor="rec-mic" style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Microphone</label>
          {micSources.length === 0 ? (
            <p className="meta" style={{ fontStyle: 'italic', margin: 0, width: 340 }}>
              {loadedSettings === null ? 'Loading microphones…' : 'No microphones detected.'}
            </p>
          ) : (
            <select
              id="rec-mic"
              value={selectedMicId ?? ''}
              disabled={busy}
              onChange={(e) => setSelectedMicId(Number(e.target.value))}
              style={{ display: 'block', width: 340 }}
            >
              {micSources.map((m) => (
                <option key={m.id} value={m.id}>{m.description}</option>
              ))}
            </select>
          )}
          {loadedSettings !== null &&
            selectedMicId !== null &&
            selectedMicId !== loadedSettings.default_mic_source_id && (
              <p className="meta" style={{ fontSize: 12, marginTop: 4, fontStyle: 'italic', width: 340 }}>
                Session override — default not changed
              </p>
            )}
          {selectedMicId !== null && (
            <div style={{ marginTop: 10, textAlign: 'left' }}>
              <span style={{ display: 'block', fontSize: 12, color: 'var(--muted)', marginBottom: 4 }}>
                Detected microphone level:
              </span>
              <MicLevel tauriSourceId={selectedMicId} width={220} height={14} />
            </div>
          )}

          <label
            style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 14, fontSize: 13 }}
            title="Leave checked for a normal call. Uncheck if several people share this device/room so Daisy separates their voices too."
          >
            <input
              type="checkbox"
              checked={singleLocalSpeaker}
              disabled={busy}
              onChange={(e) => setSingleLocalSpeaker(e.target.checked)}
            />
            <span>I'm the only person on my end</span>
          </label>
        </div>

        <div style={{ marginBottom: 22, textAlign: 'left', width: 340 }}>
          <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Tags</label>
          <p className="meta" style={{ fontSize: 12, margin: '0 0 6px' }}>
            Please select any relevant tags for this meeting — they sharpen live
            captions and transcription (names, jargon).
          </p>
          <div className="notebook__tags">
            {preRecordTags.map((t) => (
              <TagChip
                key={t.id}
                tag={t}
                onRemove={() => setPreRecordTags((p) => p.filter((x) => x.id !== t.id))}
              />
            ))}
            <TagCombobox
              excludeIds={preRecordTags.map((t) => t.id)}
              onPick={(t) => setPreRecordTags((p) => (p.some((x) => x.id === t.id) ? p : [...p, t]))}
            />
          </div>
        </div>

        <button className="btn btn--record btn--big" onClick={start} disabled={busy} style={{ minWidth: 220 }}>
          {busy ? 'Starting…' : '● Start recording'}
        </button>

        {error && (
          <p className="meta" style={{ color: 'var(--danger)', marginTop: 16, maxWidth: 340 }}>
            {error}
          </p>
        )}
      </div>
    );
  }

  // ---- recording / stopped: the Notebook layout ------------------------
  const isStopped = screen === 'stopped';
  const clockText = isStopped ? fmtElapsed(elapsedAtPause ?? elapsed) : fmtElapsed(elapsed);
  const interims: LiveTurn[] = [];
  if (interimMic) interims.push(interimMic);
  if (interimSystem) interims.push(interimSystem);
  const transcriptEmpty = finals.length === 0 && interims.length === 0;

  const head = (
    <div className="notebook__head">
      <div className="notebook__topline">
        <span
          style={{
            fontSize: 9,
            letterSpacing: '0.12em',
            textTransform: 'uppercase',
            color: 'var(--iron)',
          }}
        >
          {isStopped ? 'Paused' : 'Recording'}
        </span>
        <div style={{ flex: 1 }} />
        <span className="notebook__rec">
          <span className="notebook__rec-dot" />
          {isStopped ? 'PAUSED' : 'REC'} · {clockText}
        </span>
      </div>
      <input
        className="notebook__title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onBlur={onTitleBlur}
        placeholder="Untitled meeting"
      />
      {!isStopped && micSources.length >= 1 && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, margin: '2px 0 6px', flexWrap: 'wrap' }}>
          <span style={{ fontSize: 11, color: 'var(--iron)' }}>Mic</span>
          <select
            style={{ maxWidth: '100%' }}
            value={selectedMicId ?? ''}
            onMouseDown={refreshMicSources}
            onFocus={refreshMicSources}
            onChange={async (e) => {
              const id = Number(e.target.value);
              const prev = selectedMicId;
              setSelectedMicId(id);
              try {
                await tauri.switchRecordingMic(id);
              } catch (err) {
                setSelectedMicId(prev); // revert the dropdown if the switch failed
                setError(errStr(err));
              }
            }}
          >
            {micSources.map((m) => (
              <option key={m.id} value={m.id}>{m.description}</option>
            ))}
          </select>
          <button
            type="button"
            className={`btn ${micMuted ? 'btn--working' : ''}`}
            title={micMuted
              ? 'Mic muted — recording system audio only. Click to unmute.'
              : 'Mute your mic — record only system audio (e.g. YouTube), not your voice.'}
            onClick={async () => {
              const next = !micMuted;
              setMicMuted(next);
              try {
                await tauri.setMicMuted(next);
              } catch (err) {
                setMicMuted(!next); // revert on failure
                setError(errStr(err));
              }
            }}
          >
            {micMuted ? '🔇 Mic muted' : '🎙 Mute mic'}
          </button>
        </div>
      )}
      <div className="notebook__tags">
        {attachedIds.map((id) => {
          const t = tagsById[id];
          if (!t) return null;
          return (
            <TagChip
              key={id}
              tag={t}
              onEditPrompt={() => setPromptModalTag(t)}
              onRemove={() => removeTag(id)}
            />
          );
        })}
        <TagCombobox excludeIds={attachedIds} onPick={addTag} />
      </div>
      <textarea
        className="notebook__notes"
        value={notes}
        onChange={(e) => onNotesChange(e.target.value)}
        placeholder="My notes (markdown supported)…"
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 6, flexWrap: 'wrap' }}>
        <span aria-hidden="true" style={{ display: 'inline-flex', color: 'var(--iron)' }} title="microphone level">
          {/* Mic icon. */}
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
               strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <rect x="9" y="2" width="6" height="12" rx="3" />
            <path d="M5 11a7 7 0 0 0 14 0" />
            <line x1="12" y1="18" x2="12" y2="22" />
            <line x1="9" y1="22" x2="15" y2="22" />
          </svg>
        </span>
        <MicLevel controlledLevel={recMicLevel} width={120} height={6} />
      </div>
      {sessionId && (
        <details style={{ marginTop: 10 }}>
          <summary style={{ cursor: 'pointer', fontWeight: 600, fontSize: 13 }}>
            Ask about this meeting
          </summary>
          <div style={{ marginTop: 8 }}>
            <CallChat sessionId={sessionId} live />
          </div>
        </details>
      )}
    </div>
  );

  const transcript = (
    <div className="notebook__transcript">
      {!liveNoteDismissed && (
        <div className="live-note" role="note">
          <span>
            Live captions are a working view — speaker bleed can briefly duplicate
            words between you and the other side. The finalized transcript removes
            duplicates. Treat live text as assistance, not a record.
          </span>
          <button
            type="button"
            className="live-note__close"
            aria-label="Dismiss note"
            onClick={() => setLiveNoteDismissed(true)}
          >
            ×
          </button>
        </div>
      )}
      {liveError && (
        <p
          className="meta"
          style={{ color: 'var(--danger)', margin: '0 0 6px', fontSize: 12, fontStyle: 'italic' }}
        >
          live transcription: {liveError}
        </p>
      )}
      {transcriptEmpty && liveModeLabel !== null && (
        <p className="meta" style={{ margin: 0, fontStyle: 'italic' }}>
          waiting for live transcript…
        </p>
      )}
      {finals.map((entry, i) => {
        if (isPauseMarker(entry)) {
          return (
            <div
              key={`pause-${entry.at}-${i}`}
              className="turn turn--pause"
              style={{ color: 'var(--iron)', fontStyle: 'italic' }}
            >
              [paused at {entry.at}]
            </div>
          );
        }
        // An interim continuing the LAST paragraph flows into that bubble as
        // an italic tail — the paragraph grows in place instead of a separate
        // chunk bubble appearing and disappearing beneath it.
        const isLast = i === finals.length - 1;
        const tailTurn = isLast
          ? (entry.track === 'mic' ? interimMic : interimSystem)
          : null;
        return (
          // Stable key by (track, start_ms) — stable across polish-merge
          // index shifts.
          <LiveLine
            key={`f-${entry.track}-${entry.start_ms}`}
            turn={entry}
            tail={tailTurn?.text || undefined}
          />
        );
      })}
      {interims
        .filter((t) => {
          const last = finals[finals.length - 1];
          return !(last && !isPauseMarker(last) && last.track === t.track);
        })
        .map((t) => (
          <LiveLine key={`i-${t.track}-${t.start_ms}`} turn={t} />
        ))}
      <div ref={transcriptEndRef} />
    </div>
  );

  const captionsOffCard = (
    <div className="live-off-card" role="status">
      <p className="live-off-card__title">Live captions are off on this machine</p>
      <p className="live-off-card__body">
        Recording and transcription are still running — the full
        transcript is created automatically when you stop.
      </p>
      <AudioVisualizer level={recMicLevel} />
    </div>
  );

  return (
    <div className="notebook">
      {liveModeLabel === 'off' ? (
        <>
          <div style={{ flex: 1, minHeight: 0 }}>{head}</div>
          {captionsOffCard}
        </>
      ) : (
        <ResizableSplit
          storageKey="daisy.rec.split.v3"
          top={head}
          bottom={transcript}
          defaultFraction={0.7}
          maxFraction={0.85}
        />
      )}
      <div className="notebook__controls">
        {!isStopped ? (
          <button
            className="btn btn--big btn--stop"
            onClick={onStop}
            disabled={screen === 'stopping'}
          >
            ❚❚ {screen === 'stopping' ? 'Pausing…' : 'Pause recording'}
          </button>
        ) : (
          <>
            <button className="btn btn--big btn--resume" onClick={onResume}>
              ▶ Resume
            </button>
            <button className="btn btn--big btn--primary" onClick={() => void onProcess()}>
              ✦ Finish &amp; summarize
            </button>
          </>
        )}
        <button
          className="btn btn--big"
          onClick={() => setConfirmDiscard(true)}
          style={{ color: 'var(--danger)' }}
          title="Stop and permanently delete this recording"
        >
          🗑 Discard
        </button>
      </div>
      {error && (
        <p className="meta" style={{ color: 'var(--danger)', padding: '0 18px 12px' }}>
          {error}
        </p>
      )}
      {promptModalTag && (
        <TagPromptModal
          tag={promptModalTag}
          onClose={() => setPromptModalTag(null)}
          onSaved={(t) => setTagsById((m) => ({ ...m, [t.id]: t }))}
        />
      )}
      {confirmDiscard && (
        <ConfirmDialog
          title="Discard recording?"
          body="The audio and transcript captured so far will be permanently deleted. This can't be undone."
          confirmLabel="Discard"
          danger
          onCancel={() => setConfirmDiscard(false)}
          onConfirm={doDiscard}
        />
      )}
    </div>
  );
}
