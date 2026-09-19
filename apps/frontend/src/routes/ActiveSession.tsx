// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { memo, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { fmtHms } from '../lib/format';
import { useTags } from '../lib/useTags';
import { tauri, errStr, type AudioSourceInfo, type AudioWarning, type EventSeed, type LastTimeSuggestion, type LicenseStatus, type MachineSettings, type Tag } from '../tauri';
import { expiredBannerState } from '../lib/expired-banner';
import { ResizableSplit } from '../components/ResizableSplit';
import { CallChat } from '../components/CallChat';
import { TagChip } from '../components/tags/TagChip';
import { TagCombobox } from '../components/tags/TagCombobox';
import { TagPromptModal } from '../components/tags/TagPromptModal';
import { LastTimeChip } from '../components/tags/SuggestedTags';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { RowMenu } from '../components/RowMenu';
import { Field } from '../components/Field';
import { Select } from '../components/Select';
import { MicLevel } from '../components/MicLevel';
import { AudioVisualizer } from '../components/AudioVisualizer';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  appendPauseMarker,
  isMarker,
  isMissedSpeechMarker,
  getLiveTranscriptState,
  isPauseMarker,
  resetLiveTranscript,
  subscribeLiveTranscript,
  type LiveTurn,
} from '../liveTranscript';
import { useVisibleInterval } from '../lib/useVisibleInterval';
import { useAudioSourcesChanged } from '../lib/useAudioSourcesChanged';

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
  /** Disables the start-recording button once expired. */
  license?: LicenseStatus | null;
}

// Screen states: 'picking' = mic picker (pre-recording), 'recording' = live,
// 'starting'/'stopping' = transient backend calls, 'stopped' = paused & resumable.
type ScreenState = 'picking' | 'starting' | 'recording' | 'stopping' | 'stopped';

const NOTES_DEBOUNCE_MS = 500;
// Recording split: the top pane's max share of the screen, and how far the
// notes box boosts it while focused. The open chat panel boosts to the max.
const SPLIT_MAX_FRACTION = 0.85;
const NOTES_BOOST_FRACTION = 0.66;

/** boostFraction for the recording split: an open chat panel wins over notes focus. */
export function liveBoost(chatOpen: boolean, notesFocused: boolean): number | null {
  return chatOpen ? SPLIT_MAX_FRACTION : notesFocused ? NOTES_BOOST_FRACTION : null;
}

// Memoized. The liveTranscript store preserves object identity for unchanged
// finals across updates (push + polish-merge); the default shallow comparison
// skips re-rendering unchanged turns.
const LiveLine = memo(function LiveLine({ turn, tail }: { turn: LiveTurn; tail?: string }) {
  const cls = turn.track === 'mic' ? 'turn me' : 'turn them';
  return (
    <div className={turn.isInterim ? `${cls} turn--interim` : cls}>
      {turn.track !== 'mic' && <span className="who">Them</span>}
      <span className="turn-text" style={turn.isInterim ? { fontStyle: 'italic' } : undefined}>
        {!turn.isInterim && <span className="turn-ts">{fmtHms(Math.floor(turn.start_ms / 1000))}</span>}
        {turn.text}
        {turn.isInterim && ' …'}
        {tail != null && <em className="turn-tail"> {tail} …</em>}
      </span>
    </div>
  );
});

export function ActiveSession({ onProcessingStarted, eventSeed, onDiscarded, license }: Props) {
  const expiredBanner = expiredBannerState(license ?? null);
  const [screen, setScreen] = useState<ScreenState>('picking');
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<number | null>(null);
  const [liveModeLabel, setLiveModeLabel] = useState<string | null>(null);
  // Display preference only — the live decode runs regardless.
  const [showCaptions, setShowCaptions] = useState(true);
  useEffect(() => {
    tauri.machineSettings()
      .then((m) => setShowCaptions(m.settings.show_live_captions))
      .catch(() => {});
  }, []);
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
  const [loadedSettings, setLoadedSettings] = useState<MachineSettings | null>(null);

  // Notebook metadata (during recording).
  const [title, setTitle] = useState('');
  const [lastSavedTitle, setLastSavedTitle] = useState('');
  // The generated default (e.g. "11:24 AM"), captured once so an
  // unedited title can be told apart from a title the user typed — an
  // untouched default suggests nothing, since it carries no meeting intent.
  const initialDefaultTitle = useRef<string | null>(eventSeed ? null : '');
  // Title typed on the pre-recording screen; the backend's clock fills it in.
  const [startTitle, setStartTitle] = useState(() => eventSeed?.title ?? '');
  useEffect(() => {
    if (eventSeed) return;
    let live = true;
    tauri.defaultRecordingTitle().then((t) => {
      if (!live) return;
      initialDefaultTitle.current = t;
      setStartTitle((cur) => (cur === '' ? t : cur));
    }).catch(() => {});
    return () => { live = false; };
  }, [eventSeed]);
  // "Tags from last time" suggestion for the pre-record screen, keyed off the
  // typed title / calendar link (debounced — see the effect below).
  const [lastTime, setLastTime] = useState<LastTimeSuggestion | null>(null);
  // Tags picked on the pre-record screen. Written to the manifest at start AND
  // used to prime live captions (names/jargon) from the first chunk.
  const [preRecordTags, setPreRecordTags] = useState<Tag[]>([]);
  const [notes, setNotes] = useState('');
  const [attachedIds, setAttachedIds] = useState<string[]>([]);
  const allTags = useTags();
  const tagsById = useMemo(() => {
    const map: Record<string, Tag> = {};
    for (const t of allTags) map[t.id] = t;
    return map;
  }, [allTags]);
  const [promptModalTag, setPromptModalTag] = useState<Tag | null>(null);
  // Live-captions working-view note; per-mount (reappears on the next recording).
  const [liveNoteDismissed, setLiveNoteDismissed] = useState(false);
  // While the notes box has focus the head pane of the split grows.
  const [notesFocused, setNotesFocused] = useState(false);
  // Whether the "Ask about this meeting" chat panel is open.
  const [chatOpen, setChatOpen] = useState(false);
  const chatInputRef = useRef<HTMLInputElement>(null);
  // Opening the chat panel grows the head pane (via boostFraction below) and
  // brings the input into view once that resize has painted.
  useEffect(() => {
    if (!chatOpen) return;
    const id = requestAnimationFrame(() => {
      chatInputRef.current?.focus();
      chatInputRef.current?.scrollIntoView({ block: 'nearest' });
    });
    return () => cancelAnimationFrame(id);
  }, [chatOpen]);

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
    Promise.all([tauri.listAudioSources(), tauri.machineSettings()])
      .then(([sources, machine]) => {
        const mics = sources.filter((s) => s.kind === 'mic');
        setMicSources(mics);
        setLoadedSettings(machine.settings);
        const defaultId = machine.settings.default_mic_source_id;
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
  // Hotplug pushes from the backend refresh the list without a reopen.
  useAudioSourcesChanged(refreshMicSources);

  // A supervisor failover changes the real mic; the selector follows it.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<AudioWarning>('audio-warning', (ev) => {
      if (ev.payload.kind === 'mic-failed-over') {
        setSelectedMicId(ev.payload.id);
        refreshMicSources();
      }
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [refreshMicSources]);

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
    // Only a pending debounce carries unsaved keystrokes; without one the
    // stored notes already match, and a blur would write on every focus loss.
    if (!notesTimer.current) return;
    clearTimeout(notesTimer.current);
    notesTimer.current = null;
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

  // ---- pre-record "tags from last time" suggestion ----------------------
  // Debounced on the typed title so a session with a similar past meeting
  // (by title or, for a Calendar-seeded recording, by recurring event) can
  // surface its tags before this one even starts. An untouched default
  // title (or an empty one) carries no signal, so the effective title used
  // for matching is null in that case; with no title AND no calendar link
  // there's nothing to match against, so the fetch is skipped.
  useEffect(() => {
    if (screen !== 'picking') return;
    const trimmed = startTitle.trim();
    const effectiveTitle =
      trimmed === '' || trimmed === initialDefaultTitle.current ? null : trimmed;
    const cal = eventSeed?.calendar_link ?? null;
    if (effectiveTitle === null && cal === null) { setLastTime(null); return; }
    let cancelled = false;
    const t = setTimeout(() => {
      tauri.tagSuggestPrerecord(effectiveTitle, cal)
        .then((s) => { if (!cancelled) setLastTime(s); })
        .catch(() => { if (!cancelled) setLastTime(null); });
    }, 300);
    return () => { cancelled = true; clearTimeout(t); };
  }, [screen, startTitle, eventSeed]);

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
      appendPauseMarker(fmtHms(frozen));
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
      <div className="recorder record-card">
        <h1 className="h1">New recording</h1>

        <Field label="Title" htmlFor="rec-title">
          <input
            id="rec-title"
            data-testid="record-title"
            type="text"
            className="field__control"
            value={startTitle}
            disabled={busy}
            onChange={(e) => setStartTitle(e.target.value)}
            placeholder="Meeting title"
          />
        </Field>

        <Field label="Microphone" htmlFor="rec-mic">
          {micSources.length === 0 ? (
            <p className="meta record-card__note">
              {loadedSettings === null ? 'Loading microphones…' : 'No microphones detected.'}
            </p>
          ) : (
            <Select
              id="rec-mic"
              testId="record-mic"
              className="field__control"
              value={selectedMicId === null ? '' : String(selectedMicId)}
              disabled={busy}
              onChange={(v) => setSelectedMicId(Number(v))}
              options={micSources.map((m) => ({ value: String(m.id), label: m.description }))}
            />
          )}
          {loadedSettings !== null &&
            selectedMicId !== null &&
            selectedMicId !== loadedSettings.default_mic_source_id && (
              <p className="meta record-card__note">
                Session override — default not changed
              </p>
            )}
          {/* Only while picking: once start is in flight the preview has
              done its job, and holding the device open alongside the capture
              stream opening on it buys nothing. */}
          {selectedMicId !== null && !busy && (
            <div className="record-card__level">
              <MicLevel tauriSourceId={selectedMicId} width={220} height={14} />
            </div>
          )}
        </Field>

        <label
          className="record-card__check"
          title="Leave checked for a normal call. Uncheck if several people share this device/room so Daisy separates their voices too."
        >
          <input
            data-testid="record-single-speaker"
            type="checkbox"
            checked={singleLocalSpeaker}
            disabled={busy}
            onChange={(e) => setSingleLocalSpeaker(e.target.checked)}
          />
          <span>I&apos;m the only person on my end</span>
        </label>

        <Field
          label="Tags"
          title="Tags sharpen live captions and transcription — names, jargon, product words."
        >
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
          {lastTime && (() => {
            // No session exists yet — deselecting a suggested tag here is
            // local-only (nothing to persist); only the applied picks matter.
            const pickedIds = new Set(preRecordTags.map((t) => t.id));
            const remaining = lastTime.tag_ids.filter((id) => !pickedIds.has(id));
            if (remaining.length === 0) return null;
            return (
              <LastTimeChip
                suggestion={{ ...lastTime, tag_ids: remaining }}
                tags={allTags}
                onApply={(selected) => {
                  const byId = new Map(allTags.map((t) => [t.id, t]));
                  setPreRecordTags((prev) => [
                    ...prev,
                    ...selected.map((id) => byId.get(id)).filter((t): t is Tag => t != null),
                  ]);
                }}
              />
            );
          })()}
        </Field>

        <button
          data-testid="record-start"
          className="btn btn--record btn--big record-card__start"
          onClick={start}
          disabled={busy || expiredBanner.kind === 'show'}
          title={expiredBanner.kind === 'show' ? expiredBanner.label : undefined}
        >
          {busy ? 'Starting…' : '● Start recording'}
        </button>

        {error && <p className="meta meta--danger">{error}</p>}
      </div>
    );
  }

  // ---- recording / stopped: the Notebook layout ------------------------
  const isStopped = screen === 'stopped';
  const clockText = isStopped ? fmtHms(elapsedAtPause ?? elapsed) : fmtHms(elapsed);
  const transcriptEmpty = finals.length === 0 && !interimMic && !interimSystem;

  const head = (
    <div className="notebook__head">
      <div className="notebook__topline">
        <span className="notebook__rec">
          <span className="notebook__rec-dot" />
          {isStopped ? 'PAUSED' : 'REC'} · {clockText}
        </span>
      </div>
      <input
        data-testid="live-title"
        className="notebook__title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        onKeyDown={(e) => { if (e.key === 'Enter') e.currentTarget.blur(); }}
        onBlur={onTitleBlur}
        placeholder="Untitled meeting"
      />
      {!isStopped && micSources.length >= 1 && (
        <div style={{ display: 'flex', alignItems: 'center', gap: 8, margin: '2px 0 6px', flexWrap: 'wrap' }}>
          <span className="meta" style={{ fontSize: 'var(--t-meta)' }}>Mic</span>
          <Select
            testId="live-mic"
            label="Microphone"
            className="live-mic"
            value={selectedMicId === null ? '' : String(selectedMicId)}
            onOpen={refreshMicSources}
            options={micSources.map((m) => ({ value: String(m.id), label: m.description }))}
            onChange={async (v) => {
              const id = Number(v);
              const prev = selectedMicId;
              setSelectedMicId(id);
              try {
                await tauri.switchRecordingMic(id);
              } catch (err) {
                setSelectedMicId(prev); // revert the dropdown if the switch failed
                setError(errStr(err));
              }
            }}
          />
          <MicLevel controlledLevel={recMicLevel} width={120} height={6} />
          <button
            data-testid="live-mute"
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
            {micMuted ? 'Mic muted' : 'Mute mic'}
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
        data-testid="live-notes"
        className="notebook__notes"
        value={notes}
        onChange={(e) => onNotesChange(e.target.value)}
        onFocus={() => setNotesFocused(true)}
        onBlur={() => { setNotesFocused(false); flushNotes(); }}
        placeholder="My notes (markdown supported)…"
      />
      {sessionId && (
        <details style={{ marginTop: 10 }} open={chatOpen} onToggle={(e) => setChatOpen(e.currentTarget.open)}>
          <summary className="set-group__summary">
            Ask about this meeting
          </summary>
          <div style={{ marginTop: 8 }}>
            <CallChat sessionId={sessionId} live inputRef={chatInputRef} />
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
            data-testid="live-note-close"
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
          className="meta meta--danger"
          style={{ margin: '0 0 6px', fontSize: 'var(--t-small)', fontStyle: 'italic' }}
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
            >
              [paused at {entry.at}]
            </div>
          );
        }
        if (isMissedSpeechMarker(entry)) {
          return (
            <div key={`missed-${entry.track}-${entry.start_ms}`} className="turn turn--marker">
              [possible missed speech — {entry.track === 'mic' ? 'your mic' : 'meeting audio'} was
              faint for {Math.max(1, Math.round((entry.end_ms - entry.start_ms) / 1000))}s]
            </div>
          );
        }
        // A system interim continuing the LAST paragraph flows into that
        // bubble as an italic tail — the paragraph grows in place instead of
        // a separate chunk bubble appearing and disappearing beneath it.
        // Mic interims never tail: they render in the pending bubble below
        // until their final passes the bleed filter.
        const isLast = i === finals.length - 1;
        const tailTurn = isLast && entry.track === 'system' ? interimSystem : null;
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
      {interimSystem && (() => {
        const last = finals[finals.length - 1];
        return !(last && !isMarker(last) && last.track === 'system');
      })() && <LiveLine key={`i-system-${interimSystem.start_ms}`} turn={interimSystem} />}
      {interimMic && (
        // Mic speech is unattributed while in flight — the bleed filter only
        // runs on finals, so this could be me or an echo of them. Neutral
        // centered bubble; the final lands right (me) or vanishes (bleed).
        <div className="turn turn--pending">
          <span className="turn-text">{interimMic.text} …</span>
        </div>
      )}
      <div ref={transcriptEndRef} />
    </div>
  );

  const captionsOffCard = (
    <div className="live-off-card" role="status">
      <p className="live-off-card__title">Live transcription is unavailable</p>
      <p className="live-off-card__body">
        No speech model was found on this machine — audio is still being
        recorded and can be transcribed once a model is installed.
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
      ) : !showCaptions ? (
        <>
          <div style={{ flex: 1, minHeight: 0 }}>{head}</div>
          <div className="live-off-card" role="status">
            <p className="live-off-card__title">Live captions are hidden</p>
            <p className="live-off-card__body">
              The transcript is still being written. Show captions in
              Settings → Recordings.
            </p>
            <AudioVisualizer level={recMicLevel} />
          </div>
        </>
      ) : (
        <ResizableSplit
          storageKey="daisy.rec.split.v4"
          top={head}
          bottom={transcript}
          defaultFraction={0.42}
          maxFraction={SPLIT_MAX_FRACTION}
          boostFraction={liveBoost(chatOpen, notesFocused)}
        />
      )}
      <div className="notebook__controls">
        {!isStopped ? (
          <button
            data-testid="live-pause"
            className="btn btn--big btn--primary"
            onClick={onStop}
            disabled={screen === 'stopping'}
          >
            ❚❚ {screen === 'stopping' ? 'Pausing…' : 'Pause recording'}
          </button>
        ) : (
          <>
            <button data-testid="live-resume" className="btn btn--big btn--primary" onClick={onResume}>
              ▶ Resume
            </button>
            <button data-testid="live-finish" className="btn btn--big" onClick={() => void onProcess()}>
              ✦ Finish &amp; summarize
            </button>
          </>
        )}
        <RowMenu
          testId="live-overflow"
          label="Recording actions"
          up
          items={[{
            label: 'Discard recording',
            testId: 'live-discard',
            danger: true,
            onSelect: () => setConfirmDiscard(true),
          }]}
        />
      </div>
      {error && (
        <p className="meta meta--danger" style={{ padding: '0 18px 12px' }}>
          {error}
        </p>
      )}
      {promptModalTag && (
        <TagPromptModal
          tag={promptModalTag}
          onClose={() => setPromptModalTag(null)}
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
