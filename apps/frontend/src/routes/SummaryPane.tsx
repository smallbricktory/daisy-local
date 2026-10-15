// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  tauri,
  errStr,
  formatDurationHm,
  formatDurationSeconds,
  type Chapter,
  type IntegrationPublic,
  type SessionFocus,
  type SessionChapters,
  type SessionSpeaker,
  type SessionSummary,
  type SummaryProviderStatusKind,
  type Prompt,
  type Tag,
  type RecordingKind,
  type UndiarizedNames,
} from '../tauri';
import { TagChip } from '../components/tags/TagChip';
import { CallChat } from '../components/CallChat';
import { TagCombobox } from '../components/tags/TagCombobox';
import { GhostSuggestionChips, LastTimeChip, buildSuggested } from '../components/tags/SuggestedTags';
import { TagPromptModal } from '../components/tags/TagPromptModal';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { MarkdownView } from '../components/MarkdownView';
import { MarkdownEditor } from '../components/MarkdownEditor';
import type { JobKind } from '../lib/sessionPhase';
import { fmtDateTimeYear } from '../lib/format';
import { recordingKindLabel } from '../lib/recordingKind';
import { copyOutcomeForSize } from '../lib/copy-state';
import { copyWithPrompt, type CopyKind } from '../lib/copyWithPrompt';
import { useAiProviderStatus } from '../lib/aiProviderStatus';
import { providersPath, useNerdMode } from '../lib/nerdMode';
import { summaryButtonsDisabled } from '../lib/summary-gate';
import { runningJob, useAiJobs } from '../lib/aiJobs';
import { useFinalizing } from '../lib/finalizeRunner';
import { confirm, alert } from '../lib/confirm';
import { pushToast } from '../lib/toastStore';
import { SpeakerLabeler, speakerNeedsReview } from '../components/SpeakerLabeler';
import { EmptyState } from '../components/EmptyState';
import { useSessionData } from '../lib/useSessionData';
import { useTagSuggestions } from '../lib/useTagSuggestions';
import { useSessionPhase } from '../lib/sessionLifecycle';
import { useDismiss } from '../lib/useDismiss';
import { sleepNotices, type SegmentLoose } from '../lib/sleepNotice';
import {
  subscribeLiveTranscript,
  getLiveTranscriptState,
  isMarker,
  labelLines,
  namedTurns,
  type LabelledLine,
  type LiveTurn,
} from '../liveTranscript';

type Tab = 'summary' | 'notes' | 'transcript' | 'participants' | 'chat';

/** Keystrokes settle for this long before the notes are written. */
const NOTES_DEBOUNCE_MS = 500;

interface ManifestLoose {
  recording_kind?: RecordingKind | null;
  created_at_unix_seconds?: number;
  finalized_at_unix_seconds?: number;
  chunks?: { duration_seconds?: number | null }[];
  diarization_unavailable?: boolean;
  recording_segments?: SegmentLoose[];
}

function fmtDateTime(unixSec: number | undefined): string {
  if (!unixSec) return '—';
  return fmtDateTimeYear(unixSec);
}

// Recording length = sum of chunk durations from the manifest, formatted as
// coarse "Xh Ym" for the detail header.
function fmtDurationHm(m?: ManifestLoose | null): string {
  const total = (m?.chunks ?? []).reduce((acc, c) => acc + (c?.duration_seconds ?? 0), 0);
  return formatDurationHm(total);
}

// render_markdown() emits "# {title}\n\n**TL;DR.** {tldr}\n\n## ...". Drops
// everything up to and including the leading "**TL;DR.**" line; input without
// such a line (hand-edited markdown) is returned as-is.
function summaryBody(md: string): string {
  const m = md.match(/\*\*TL;DR\.?\*\*[^\n]*\n+/i);
  return m && m.index !== undefined ? md.slice(m.index + m[0].length).trimStart() : md;
}

interface Turn { side: 'me' | 'them' | null; ts: string | null; who: string | null; text: string }
/** One transcript line inside a merged paragraph, with its own timestamp. */
interface TurnPart { ts: string | null; text: string }
interface MergedTurn extends Turn { parts: TurnPart[] }

function hmsToSeconds(hms: string): number {
  const [h, m, s] = hms.split(':').map(Number);
  return (h || 0) * 3600 + (m || 0) * 60 + (s || 0);
}

function parseTranscript(md: string): Turn[] {
  const out: Turn[] = [];
  // Matches `[HH:MM:SS] **Name**: text` or `**Name**: text` (or bare `Name: text`).
  // Name is whatever sits inside the leading `**...**` block, or before the
  // first colon when unbolded.
  const RE = /^(?:\[(\d{2}:\d{2}:\d{2})\]\s*)?(?:\*\*([^*]+?)\*\*|([^:]+?)):\s*(.*)$/;
  for (const line of md.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    // Skips the header block render_markdown emits: "# Transcript: …",
    // "## Chunk N", "_Provider: …_".
    if (trimmed.startsWith('#') || /^_.*_$/.test(trimmed)) continue;
    const m = trimmed.match(RE);
    if (m) {
      const who = (m[2] ?? m[3] ?? '').trim();
      out.push({
        ts: m[1] ?? null,
        who,
        side: who === 'Me' ? 'me' : 'them',
        text: m[4],
      });
    } else {
      out.push({ ts: null, who: null, side: null, text: trimmed });
    }
  }
  return out;
}

/** Deterministic background tint per non-"Me" speaker: the speaker name is
 *  hashed to pick from a small muted palette. */
/** Per-speaker hues, passed to the stylesheet as `--turn-tint` and mixed
 *  against the theme's surface there — never applied as a finished colour. */
const THEM_PALETTE = [
  '#F1E5D8', // sand
  '#E5EDE2', // sage tint
  '#E1E6F0', // periwinkle
  '#EFE0E8', // dusty rose
  '#E6EDEC', // mist
  '#EEE3D4', // sand 2
];
function speakerTint(who: string | null): string | null {
  if (!who || who === 'Me') return null;
  let h = 0;
  for (let i = 0; i < who.length; i++) h = (h * 31 + who.charCodeAt(i)) >>> 0;
  return THEM_PALETTE[h % THEM_PALETTE.length];
}

/** Room/Remote badge label for a transcript turn, or null when the speaker has
 *  no known side (the local user "Me", or an unmapped name). Side comes from
 *  the session's diarized speakers (mic = Room, system = Remote). */
export function sideBadge(who: string | null, nameToSide: Record<string, 'room' | 'remote'>): string | null {
  if (!who || who === 'Me') return null;
  const side = nameToSide[who];
  if (!side) return null;
  return side === 'room' ? 'Room' : 'Remote';
}

// HTMLMediaElement.error.code → human-friendly label. Codes are from the
// MediaError spec (1=MEDIA_ERR_ABORTED, 2=NETWORK, 3=DECODE, 4=SRC_NOT_SUPPORTED).
const ERR_LABEL: Record<number, string> = {
  1: 'aborted',
  2: 'network error',
  3: 'decode error',
  4: 'source not supported / not found',
};

/** Title on every summary/chapters button gated by someone else's AI job. */
const AI_BUSY_TITLE = 'Another AI task is still running';

interface SummaryPaneProps {
  sessionId: string;
  onMetaChanged?: () => void;
  /** Starts a background finalize/summarize job in App.tsx. */
  onStartSummarize: (sessionId: string, title: string, kind?: JobKind, promptId?: string) => void;
  /** True while App.tsx has a background job running for this session.
   *  Drives the disabled state of Generate/Regenerate. */
  isProcessing?: boolean;
  /** Fired after a successful delete. Library refreshes its list and clears
   *  the selection. */
  onDeleted?: (sessionId: string) => void;
  /** Navigate to the AI provider picker. Used by inline hints when no summary
   *  provider is configured. */
  onNavigateToProviders?: () => void;
  /** Deep-link target from a search result: which tab to open and (for the
   *  transcript) a query to scroll to + highlight. */
  focus?: SessionFocus;
}

export function SummaryPane({ sessionId, onMetaChanged, onStartSummarize, isProcessing = false, onDeleted, onNavigateToProviders, focus }: SummaryPaneProps) {
  const { view, meta, summary, chapters, speakers, tags, loadErr, reload } = useSessionData(sessionId);

  // Ghost chips and the last-time chip only show while the session has no
  // tags yet — once a tag lands, the inline suggestion row gives way to the
  // applied chips. The picker's Suggested section stays available either way
  // (see `suggestedForPicker` below).
  const suggestions = useTagSuggestions(sessionId);
  // `meta` is null while the session is loading.
  const appliedTagIds = useMemo(() => meta?.tag_ids ?? [], [meta]);
  const visibleVocab = appliedTagIds.length === 0 ? (suggestions?.vocab ?? []) : [];
  const lastTime = appliedTagIds.length === 0 ? suggestions?.last_time ?? null : null;
  const suggestedForPicker = useMemo(
    () => buildSuggested(suggestions, tags, appliedTagIds),
    [suggestions, tags, appliedTagIds],
  );

  // True while THIS session is finalizing (transcribe/dedup/…), parked at the
  // label gate, or interrupted with no transcript.md yet. The live transcript
  // stays on screen during these phases; the data is on disk in
  // live_transcript.jsonl regardless of phase.
  const lifecyclePhase = useSessionPhase();
  const sessionFinalizing =
    (lifecyclePhase.kind === 'finalizing'
      || lifecyclePhase.kind === 'needs-labels'
      || lifecyclePhase.kind === 'interrupted')
    && 'sessionId' in lifecyclePhase
    && lifecyclePhase.sessionId === sessionId;

  const [tab, setTab] = useState<Tab>('summary');
  const [editing, setEditing] = useState(false);
  const [editText, setEditText] = useState('');
  const [notesText, setNotesText] = useState<string | null>(null);
  const notesTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const notesRef = useRef('');
  const nerd = useNerdMode();
  const [localBusy, setLocalBusy] = useState(false);
  // A background job (cascade / regen-summary / regen-transcript) for this
  // session also counts as busy. `useFinalizing` tracks the finalizeRunner's
  // in-flight set.
  const finalizing = useFinalizing(sessionId);
  // The backend registry, which every window and every session reads.
  const aiJobs = useAiJobs();
  const summaryJobHere = runningJob(aiJobs, sessionId, 'summary') != null;
  const chaptersJobHere = runningJob(aiJobs, sessionId, 'chapters') != null;
  // Any other AI task holds the worker, so this session's buttons are gated.
  const summaryGated = aiJobs.length > 0 && !summaryJobHere;
  const chaptersGated = aiJobs.length > 0 && !chaptersJobHere;
  const busy = localBusy || isProcessing || finalizing || summaryJobHere;
  const setBusy = setLocalBusy;
  // A task for this session leaving the registry means new summary/chapters
  // on disk, including one started before this pane mounted.
  const jobsHere = aiJobs.filter((j) => j.session_id === sessionId).length;
  const priorJobsHere = useRef({ sessionId, count: jobsHere });
  useEffect(() => {
    const prior = priorJobsHere.current;
    priorJobsHere.current = { sessionId, count: jobsHere };
    if (prior.sessionId === sessionId && prior.count > jobsHere) void reload();
  }, [sessionId, jobsHere, reload]);
  const [err, setErr] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  // Remount key for <CallChat>; bumped after deleting the chat.
  const [chatNonce, setChatNonce] = useState(0);
  // Whether a persisted chat thread exists; gates the Delete chat action.
  // Seeded here (the chat tab may never mount), kept fresh by CallChat.
  const [hasChat, setHasChat] = useState(false);
  useEffect(() => {
    let cancelled = false;
    setHasChat(false);
    tauri.liveChatLoad(sessionId)
      .then((c) => { if (!cancelled) setHasChat(c.messages.length > 0); })
      .catch(() => { /* no thread */ });
    return () => { cancelled = true; };
  }, [sessionId]);
  const [editingTitle, setEditingTitle] = useState(false);
  const [editingDate, setEditingDate] = useState(false);
  const [editDateVal, setEditDateVal] = useState('');
  const [editTimeVal, setEditTimeVal] = useState('');
  const [promptModalTag, setPromptModalTag] = useState<Tag | null>(null);
  const [audioUrl, setAudioUrl] = useState<string | null>(null);
  // Counts loads of the playback audio. Keys the <audio> element.
  const [audioLoads, setAudioLoads] = useState(0);
  const [audioError, setAudioError] = useState<string | null>(null);
  // Shows a loaded URL and clears the "can't play" state.
  const showAudio = useCallback((url: string | null) => {
    setAudioUrl(url);
    setAudioError(null);
    setAudioLoads((n) => n + 1);
  }, []);
  const sessionIdRef = useRef(sessionId);
  sessionIdRef.current = sessionId;
  const audioRef = useRef<HTMLAudioElement | null>(null);

  const ai = useAiProviderStatus();
  const providerStatus = useMemo(
    () => (ai.state === 'loading' ? null : { state: ai.state, provider: ai.provider, hint: ai.hint }),
    [ai],
  );
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  // Per-regenerate style override; undefined = use the global default style.
  useEffect(() => {
    void tauri.listPrompts().then(setPrompts).catch(() => {});
  }, []);
  // True once the user has manually clicked a tab; suppresses auto-routing.
  // Reset whenever sessionId changes.
  const [userPickedTab, setUserPickedTab] = useState(false);
  function pickTab(t: Tab) { setUserPickedTab(true); setTab(t); }
  // Query to scroll to + highlight, set when arriving from a search result,
  // else null. Consumed by the transcript turn-flash and the summary
  // block-flash effect below.
  const [focusQuery, setFocusQuery] = useState<string | null>(null);
  const [focusSeekMs, setFocusSeekMs] = useState<number | null>(null);
  // Scrollable summary container — the block-flash effect searches inside it.
  const paneRef = useRef<HTMLDivElement>(null);
  // First successful load per session triggers the auto-route once; later
  // library:changed reloads do not re-route.
  const routedForSession = useRef<string | null>(null);

  // Loads meeting.opus. On Linux the player reads the file from its file://
  // URL; macOS and Windows get the IPC bytes as a Blob URL, revoked once the
  // player has moved off it (effect below).
  const loadPlaybackAudio = useCallback(
    async (sid: string): Promise<string | null> => {
      const exists = await tauri.sessionHasPlaybackAudio(sid).catch(() => false);
      if (!exists) return null;
      const fileUrl = await tauri.sessionPlaybackAudioUrl(sid);
      if (fileUrl) return fileUrl;
      // The backend returns raw Ogg-Opus on Windows and an 8 kHz µ-law
      // WAV on macOS. The magic bytes are sniffed to set the Blob MIME.
      const bytes = await tauri.sessionPlaybackAudioBytes(sid);
      const sig = new Uint8Array(bytes.slice(0, 4));
      const isRiff = sig[0] === 0x52 && sig[1] === 0x49 && sig[2] === 0x46 && sig[3] === 0x46; // "RIFF"
      const type = isRiff ? 'audio/wav' : 'audio/ogg; codecs=opus';
      const blob = new Blob([bytes], { type });
      return URL.createObjectURL(blob);
    },
    [],
  );

  // Reloads the player after meeting.opus was rebuilt. The result applies
  // only while `sid` is still the open session; a failed reload keeps the
  // current URL.
  const reloadAudio = useCallback(
    (sid: string) => {
      loadPlaybackAudio(sid)
        .then((url) => {
          if (sessionIdRef.current !== sid) {
            if (url?.startsWith('blob:')) URL.revokeObjectURL(url);
            return;
          }
          showAudio(url);
        })
        .catch((e) => console.warn('playback audio reload failed', { sessionId: sid, error: e }));
    },
    [loadPlaybackAudio, showAudio],
  );

  useEffect(() => {
    // New session — reset local UI only; useSessionData owns the data fetch.
    setUserPickedTab(false);
    setTab('summary');
    setEditing(false);
    setErr(null);
    setNotesText(null);
    setEditingTitle(false);
    setAudioUrl(null);
    setAudioError(null);
    let cancelled = false;
    loadPlaybackAudio(sessionId).then((url) => {
      if (cancelled) {
        if (url?.startsWith('blob:')) URL.revokeObjectURL(url);
      } else {
        showAudio(url);
      }
    }).catch((e) => {
      console.warn('playback audio load failed', { sessionId, error: e });
      if (!cancelled) setAudioError(errStr(e));
    });
    return () => { cancelled = true; };
  }, [sessionId, loadPlaybackAudio, showAudio]);

  // Reloads playback audio when the session's progress channel reports `done`.
  // Transcript/summary/speaker data reloads come from library:changed via
  // useSessionData, not from here.
  useEffect(() => {
    let un: UnlistenFn | undefined;
    listen<{ stage: string }>(`daisy://session/${sessionId}/progress`, (ev) => {
      if (ev.payload.stage === 'done') reloadAudio(sessionId);
    }).then((u) => { un = u; });
    return () => { if (un) un(); };
  }, [sessionId, reloadAudio]);

  // Revokes a Blob URL after the commit that replaces or removes it, when the
  // <audio> element no longer reads it.
  useEffect(() => {
    if (!audioUrl?.startsWith('blob:')) return;
    return () => URL.revokeObjectURL(audioUrl);
  }, [audioUrl]);

  // Auto-picks the initial tab on the first successful load of a session only,
  // never on a library:changed reload. Suppressed once the user clicks a tab.
  useEffect(() => {
    if (userPickedTab) return;
    if (!view || !providerStatus || speakers == null) return;
    if (routedForSession.current === sessionId) return;
    routedForSession.current = sessionId;
    const next = pickInitialTab(speakers, summary, providerStatus);
    setTab((cur) => (cur === next ? cur : next));
  }, [view, providerStatus, speakers, summary, sessionId, userPickedTab]);

  // "Open" from a summary-ready / done toast forces the Summary tab — even when
  // we're already on this session's detail page looking at another tab.
  useEffect(() => {
    const onShow = (ev: Event) => {
      const id = (ev as CustomEvent<{ sessionId: string }>).detail?.sessionId;
      if (id === sessionId) {
        setUserPickedTab(true); // suppress the tab auto-pick
        setTab('summary');
      }
    };
    window.addEventListener('daisy:show-summary', onShow as EventListener);
    return () => window.removeEventListener('daisy:show-summary', onShow as EventListener);
  }, [sessionId]);

  // Deep-link from a search result: opens the requested tab and, for the
  // transcript, stashes the query TranscriptTab scrolls to + highlights.
  // Suppresses the auto-pick. Declared after the session-reset effect; the
  // later effect wins the initial tab.
  useEffect(() => {
    if (!focus) { setFocusQuery(null); setFocusSeekMs(null); return; }
    setUserPickedTab(true);
    routedForSession.current = sessionId; // make the auto-pick heuristic inert
    if (focus.tab) setTab(focus.tab);
    setFocusQuery(focus.query ?? null);
    setFocusSeekMs(focus.seekMs ?? null);
  }, [focus, sessionId]);

  // Cited-moment arrival: cue the player to the citation's offset once the
  // audio element exists. Seeking again on loadedmetadata covers the race
  // where currentTime is set before the source reports seekable ranges.
  useEffect(() => {
    if (focusSeekMs == null || !audioUrl) return;
    const a = audioRef.current;
    if (!a) return;
    const secs = focusSeekMs / 1000;
    a.currentTime = secs;
    const onMeta = () => { a.currentTime = secs; };
    a.addEventListener('loadedmetadata', onMeta);
    return () => a.removeEventListener('loadedmetadata', onMeta);
  }, [focusSeekMs, audioUrl]);

  // Summary tab: flashes the first markdown block containing the query on
  // arrival from a search result. Transcript has its own turn-flash in
  // TranscriptTab; notes is an editable textarea (no flash). Adds a transient
  // class to an existing element.
  useEffect(() => {
    if (tab !== 'summary' || !focusQuery) return;
    const root = paneRef.current;
    if (!root) return;
    const toks = focusQuery.toLowerCase().replace(/['"]/g, ' ').split(/\s+/).filter(Boolean);
    if (toks.length === 0) return;
    const blocks = root.querySelectorAll<HTMLElement>('.markdown-view :is(p, li, h1, h2, h3)');
    const target = Array.from(blocks).find((el) => {
      const lc = (el.textContent ?? '').toLowerCase();
      return toks.some((t) => lc.includes(t));
    });
    if (!target) return;
    const raf = requestAnimationFrame(() => target.scrollIntoView({ block: 'center', behavior: 'smooth' }));
    target.classList.add('md-found');
    const clr = setTimeout(() => target.classList.remove('md-found'), 2400);
    return () => { cancelAnimationFrame(raf); clearTimeout(clr); target.classList.remove('md-found'); };
  }, [tab, focusQuery, summary, view]);

  function seek(seconds: number) {
    const a = audioRef.current;
    if (a) {
      a.currentTime = seconds;
      a.play().catch(() => {});
    }
  }

  useEffect(() => {
    if (tab === 'notes' && notesText === null) {
      tauri.sessionNotesLoad(sessionId).then(setNotesText).catch(() => setNotesText(''));
    }
  }, [tab, notesText, sessionId]);

  // A pending write is flushed when the session changes, the pane unmounts
  // or the window goes hidden; a failed flush surfaces as a toast.
  useEffect(() => {
    const sid = sessionId;
    const flush = () => {
      if (!notesTimer.current) return;
      clearTimeout(notesTimer.current);
      notesTimer.current = null;
      tauri.sessionNotesSave(sid, notesRef.current).catch((e) => {
        console.error('notes save failed', e);
        pushToast({ id: `notes-save-${sid}`, severity: 'error', title: 'Notes could not be saved', body: errStr(e) });
      });
    };
    const onHidden = () => { if (document.visibilityState === 'hidden') flush(); };
    document.addEventListener('visibilitychange', onHidden);
    return () => {
      document.removeEventListener('visibilitychange', onHidden);
      flush();
    };
  }, [sessionId]);

  function onNotesChange(value: string): void {
    setNotesText(value);
    notesRef.current = value;
    if (notesTimer.current) clearTimeout(notesTimer.current);
    notesTimer.current = setTimeout(() => {
      notesTimer.current = null;
      tauri.sessionNotesSave(sessionId, value).catch((e) => setErr(errStr(e)));
    }, NOTES_DEBOUNCE_MS);
  }

  if (loadErr) return <div className="summary-pane"><p className="meta--danger">Failed to load: {loadErr}</p></div>;
  if (!view || !meta) return <div className="summary-pane"><p className="meta">Loading…</p></div>;

  const manifest = (view.manifest_json ?? null) as ManifestLoose | null;
  const kind = manifest?.recording_kind;
  const importedTag = kind ? ` · ${recordingKindLabel(kind).chip.charAt(0)}${recordingKindLabel(kind).chip.slice(1).toLowerCase()}` : '';
  const eyebrowTail = summary
    ? `${fmtDurationHm(manifest)}${importedTag}`
    : `${fmtDurationHm(manifest)} · not summarized yet${importedTag}`;
  const others = meta.attendees.filter((a) => a.role === 'other').map((a) => a.display_name);

  // Separate date + time inputs, committed only by an explicit Save.
  const pad2 = (n: number) => String(n).padStart(2, '0');
  const toDateInput = (unixSec: number): string => {
    const d = new Date(unixSec * 1000);
    return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
  };
  const toTimeInput = (unixSec: number): string => {
    const d = new Date(unixSec * 1000);
    return `${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  };
  const saveDate = async () => {
    if (!editDateVal) { setEditingDate(false); return; }
    const ts = Math.floor(new Date(`${editDateVal}T${editTimeVal || '00:00'}`).getTime() / 1000);
    setEditingDate(false);
    if (!Number.isFinite(ts) || ts === (manifest?.created_at_unix_seconds ?? 0)) return;
    try {
      await tauri.sessionMetaUpdate({ session_id: sessionId, created_at_unix_seconds: ts });
      await reload();
      onMetaChanged?.();
    } catch (er) { setErr(errStr(er)); }
  };

  return (
    <div className="summary-pane" ref={paneRef}>
      <div className="summary-pane__eyebrow">
        {editingDate && manifest?.created_at_unix_seconds ? (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6, flexWrap: 'wrap' }}>
            <input
              data-testid="session-when-date"
              type="date"
              autoFocus
              value={editDateVal}
              onChange={(e) => setEditDateVal(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Escape') setEditingDate(false); if (e.key === 'Enter') void saveDate(); }}
              style={{ colorScheme: 'light' }}
            />
            <input
              data-testid="session-when-time"
              type="time"
              value={editTimeVal}
              onChange={(e) => setEditTimeVal(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Escape') setEditingDate(false); if (e.key === 'Enter') void saveDate(); }}
              style={{ colorScheme: 'light' }}
            />
            <button data-testid="session-when-save" className="btn btn--primary" onClick={() => void saveDate()}>Save</button>
            <button data-testid="session-when-cancel" className="btn" onClick={() => setEditingDate(false)}>Cancel</button>
          </span>
        ) : (
          <span
            data-testid="session-when"
            role="button"
            title="Click to change the recording date & time"
            style={{ cursor: 'pointer', textDecorationLine: 'underline', textDecorationStyle: 'dotted', textUnderlineOffset: 3 }}
            onClick={() => {
              const ts = manifest?.created_at_unix_seconds;
              if (ts) { setEditDateVal(toDateInput(ts)); setEditTimeVal(toTimeInput(ts)); }
              setEditingDate(true);
            }}
          >
            {fmtDateTime(manifest?.created_at_unix_seconds)}
          </span>
        )}
        {' · '}{eyebrowTail}
      </div>

      <div className="summary-pane__title-row">
        {editingTitle ? (
          <input
            data-testid="session-title"
            className="summary-pane__title summary-pane__title--edit"
            autoFocus
            defaultValue={meta.title ?? ''}
            onKeyDown={(e) => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); if (e.key === 'Escape') setEditingTitle(false); }}
            onBlur={async (e) => {
              const v = e.target.value.trim();
              try {
                await tauri.sessionMetaUpdate({ session_id: sessionId, title: v || null });
                await reload();
                onMetaChanged?.();
              } catch (er) { setErr(errStr(er)); }
              setEditingTitle(false);
            }}
          />
        ) : (
          <h1 data-testid="session-title" className="summary-pane__title" style={{ cursor: 'text', flex: 1, margin: 0 }} title="Click to rename" onClick={() => setEditingTitle(true)}>
            {meta.title || fmtDateTime(manifest?.created_at_unix_seconds)}
          </h1>
        )}
        {/* Every action on this page ends on the same edge: Copy acts on the
            open tab, Action opens everything else. */}
        {tab === 'summary' && summary && (
          <CopyButton kind="summary" text={summary.markdown} />
        )}
        {tab === 'transcript' && view.transcript_md && (
          <CopyButton kind="transcript" text={view.transcript_md} />
        )}
        <ActionMenu
          hasSummary={summary != null}
          busy={busy}
          gated={summaryGated}
          providerStatus={providerStatus}
          onEditSummary={summary ? () => { setEditText(summary.markdown); setEditing(true); setTab('summary'); } : undefined}
          prompts={prompts}
          onRegenSummary={summary ? async (promptId?: string) => {
            if (summary.user_edited) {
              const ok = await confirm({
                title: 'Regenerate summary?',
                body: 'This will overwrite your hand-edited summary.',
                confirmLabel: 'Regenerate', danger: true,
              });
              if (!ok) return;
            }
            setErr(null);
            onStartSummarize(sessionId, meta.title || sessionId, 'regen-summary', promptId);
          } : undefined}
          onRegenTranscript={async () => {
            const ok = await confirm({
              title: 'Regenerate transcript?',
              body: 'This will overwrite the existing transcript and re-run dedup.',
              confirmLabel: 'Regenerate', danger: true,
            });
            if (!ok) return;
            setErr(null);
            onStartSummarize(sessionId, meta.title || sessionId, 'regen-transcript');
          }}
          onPolishTranscript={async () => {
            const ok = await confirm({
              title: 'Polish transcript?',
              body: 'Sends the transcript to your AI provider to clean up punctuation/casing and redact secrets, then overwrites the transcript text.',
              confirmLabel: 'Polish',
            });
            if (!ok) return;
            setErr(null);
            onStartSummarize(sessionId, meta.title || sessionId, 'polish-transcript');
          }}
          onRepair={async () => {
            setErr(null);
            try {
              const n = await tauri.repairSession(sessionId);
              await reload();
              if (n === 0) setErr('Nothing to repair — all files present.');
            } catch (e) { setErr(errStr(e)); }
          }}
          onExportMd={summary ? () => exportSummaryMd(summary.markdown, meta.title || sessionId) : undefined}
          sentIntegrationIds={meta.sent_integration_ids}
          onSendTo={async (integrationId) => {
            setErr(null);
            try {
              await tauri.integrationPush(sessionId, integrationId);
              await reload();
            } catch (e) { setErr(errStr(e)); }
          }}
          onDelete={() => setConfirmDelete(true)}
          onDeleteChat={!hasChat ? undefined : async () => {
            const ok = await confirm({
              title: 'Delete this chat?',
              body: 'The in-call chat for this meeting will be permanently deleted. The recording, transcript, and summary are kept.',
              confirmLabel: 'Delete', danger: true,
            });
            if (!ok) return;
            setErr(null);
            try {
              await tauri.liveChatDelete(sessionId);
              setHasChat(false);
              setChatNonce((n) => n + 1); // remount CallChat
            } catch (e) { setErr(errStr(e)); }
          }}
        />
      </div>

      {sleepNotices(manifest?.recording_segments).map((text) => (
        <p key={text} className="meta" data-testid="session-sleep-notice">{text}</p>
      ))}

      <div className="summary-pane__meta">
        {meta.tag_ids.map((id) => {
          const tag = tags.find((t) => t.id === id);
          if (!tag) return null;
          return (
            <TagChip
              key={id}
              tag={tag}
              onEditPrompt={() => setPromptModalTag(tag)}
              onRemove={async () => {
                try {
                  await tauri.sessionAssignTags(sessionId, meta.tag_ids.filter((x) => x !== id));
                  await reload();
                  onMetaChanged?.();
                } catch (er) { setErr(errStr(er)); }
              }}
            />
          );
        })}
        {lastTime && (
          <LastTimeChip
            suggestion={lastTime}
            tags={tags}
            onApply={async (selectedIds, deselectedIds) => {
              try {
                await tauri.sessionApplyTagSuggestions(
                  sessionId,
                  [...meta.tag_ids, ...selectedIds],
                  deselectedIds,
                );
                await reload();
                onMetaChanged?.();
              } catch (er) { setErr(errStr(er)); }
            }}
          />
        )}
        <GhostSuggestionChips
          suggestions={visibleVocab}
          tags={tags}
          onAccept={async (tagId) => {
            try {
              await tauri.sessionAssignTags(sessionId, [...meta.tag_ids, tagId]);
              await reload();
              onMetaChanged?.();
            } catch (er) { setErr(errStr(er)); }
          }}
        />
        <TagCombobox
          excludeIds={meta.tag_ids}
          suggested={suggestedForPicker}
          onDismissSuggested={async (tagId) => {
            try {
              await tauri.sessionDismissTagSuggestions(sessionId, [tagId]);
              const name = tags.find((t) => t.id === tagId)?.name ?? 'tag';
              pushToast({
                id: `tag-suggest-undo-${sessionId}-${tagId}`,
                severity: 'info',
                title: `Dismissed suggestion “${name}”`,
                actions: [{
                  label: 'Undo',
                  primary: true,
                  onClick: () => { void tauri.sessionUndismissTagSuggestions(sessionId, [tagId]); },
                }],
                autoDismissMs: 6000,
              });
            } catch (er) { setErr(errStr(er)); }
          }}
          onPick={async (t) => {
            try {
              await tauri.sessionAssignTags(sessionId, [...meta.tag_ids, t.id]);
              await reload();
              onMetaChanged?.();
            } catch (er) { setErr(errStr(er)); }
          }}
        />
        {others.length > 0 && <span style={{ marginLeft: 'auto' }}>w/ {others.join(', ')}</span>}
      </div>

      {(audioUrl || audioError) && (
        <div className="playback">
          {audioUrl && <audio
            key={audioLoads}
            ref={audioRef}
            data-testid="session-playback"
            controls
            preload="auto"
            src={audioUrl}
            className="playback__audio"
            onError={(e) => {
              const el = e.currentTarget;
              const code = el.error?.code ?? 0;
              const msg = el.error?.message ?? '';
              const label = ERR_LABEL[code] ?? `unknown (${code})`;
              const detail = msg ? `${label}: ${msg}` : label;
              console.error('audio load failed', { sessionId, src: audioUrl, code, message: msg });
              setAudioError(detail);
            }}
          />}
          {audioError && (
            <span className="playback__err" title={audioError}>
              can't play — {audioError}
            </span>
          )}
        </div>
      )}

      <div className="detail-tabs">
        {(['summary', 'notes', 'transcript', 'participants', 'chat'] as Tab[]).map((t) => (
          <span key={t} data-testid={`session-tab-${t}`} className={`detail-tabs__tab ${tab === t ? 'detail-tabs__tab--on' : ''}`} onClick={() => pickTab(t)}>
            {t}
          </span>
        ))}
      </div>

      {tab === 'summary' && (
        <SummaryTab
          summary={summary}
          editing={editing}
          editText={editText}
          busy={busy}
          gated={summaryGated}
          err={err}
          providerStatus={providerStatus}
          onNavigateToProviders={onNavigateToProviders}
          setEditing={setEditing}
          setEditText={setEditText}
          onGenerate={() => {
            setErr(null);
            // 'regen-summary' surfaces a toast (start → ready/error) and
            // re-runs the summary when a transcript already exists, falling
            // back to a full cascade when there is none.
            onStartSummarize(sessionId, meta.title || sessionId, 'regen-summary');
          }}
          onSaveEdit={async () => {
            setBusy(true); setErr(null);
            try { await tauri.summarySaveEdit(sessionId, editText); await reload(); setEditing(false); }
            catch (e) { setErr(errStr(e)); }
            finally { setBusy(false); }
          }}
        />
      )}

      {tab === 'notes' && (
        <div>
          <MarkdownEditor value={notesText ?? ''} onChange={onNotesChange} minHeight={260} />
          {err && <p data-testid="session-error" className="meta meta--danger">{err}</p>}
        </div>
      )}

      {tab === 'chat' && (
        <div style={{ padding: '4px 2px' }}>
          <CallChat key={chatNonce} sessionId={sessionId} live={false} onThreadChange={setHasChat} />
        </div>
      )}

      {tab === 'transcript' && (
        <>
          <ChaptersTOC
            chapters={chapters}
            sessionId={sessionId}
            onSeek={audioUrl ? seek : undefined}
            onChanged={reload}
            disabled={busy}
            hasTranscript={!!view.transcript_md?.trim()}
            gated={chaptersGated}
            jobRunning={chaptersJobHere}
            providerStatus={providerStatus}
            onNavigateToProviders={onNavigateToProviders}
          />
          <TranscriptTab
            md={view.transcript_md}
            sessionId={sessionId}
            onSeek={audioUrl ? seek : undefined}
            finalizing={sessionFinalizing || finalizing}
            focusQuery={focusQuery}
            focusSeekMs={focusSeekMs}
            nameToSide={Object.fromEntries((speakers ?? []).map((s) => [s.display_name, s.side]))}
            onTruncated={() => {
              reload();
              onMetaChanged?.();
              // The edit rebuilds meeting.opus; the player reloads it.
              reloadAudio(sessionId);
            }}
          />
        </>
      )}

      {tab === 'participants' && (
        <SpeakerLabeler
          sessionId={sessionId}
          sessionTitle={meta?.title ?? null}
          diarizationUnavailable={!!manifest?.diarization_unavailable}
          recordingKind={manifest?.recording_kind ?? null}
          inviteAttendees={meta?.attendees ?? []}
          onChanged={async () => {
            // Backend mutations rerender transcript.md + emit library:changed;
            // useSessionData reloads in place.
            await reload();
          }}
        />
      )}

      {promptModalTag && (
        <TagPromptModal tag={promptModalTag} onClose={() => setPromptModalTag(null)} onSaved={async () => { setPromptModalTag(null); await reload(); }} />
      )}
      {confirmDelete && (
        <ConfirmDialog
          title="Delete this recording?"
          body={
            <>
              <p>
                The audio, transcript, summary, notes, and tags for{' '}
                <strong>{meta.title || sessionId}</strong> will be permanently deleted.
              </p>
              <p>This can't be undone.</p>
            </>
          }
          confirmLabel="Delete"
          danger
          typedConfirm="DELETE"
          onCancel={() => setConfirmDelete(false)}
          onConfirm={async () => {
            await tauri.deleteSession(sessionId);
            setConfirmDelete(false);
            onDeleted?.(sessionId);
          }}
        />
      )}
      {nerd && (
        <p className="summary-pane__sid">
          <em>{sessionId}</em>
        </p>
      )}
    </div>
  );
}

function SummaryTab(props: {
  summary: SessionSummary | null;
  editing: boolean;
  editText: string;
  busy: boolean;
  /** An AI task for another session or another kind holds the worker. */
  gated: boolean;
  err: string | null;
  providerStatus: { state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null;
  onNavigateToProviders?: () => void;
  setEditing: (b: boolean) => void;
  setEditText: (s: string) => void;
  onGenerate: () => void;
  onSaveEdit: () => void;
}) {
  const { summary, editing, editText, busy, gated, err, providerStatus, onNavigateToProviders, setEditing, setEditText, onGenerate, onSaveEdit } = props;

  const providerMissing = providerStatus?.state === 'Missing';
  const providerUnreachable = providerStatus?.state === 'Unreachable';
  const providerUnsupported = providerStatus?.state === 'Unsupported';
  const providerLocked = providerStatus?.state === 'VaultLocked';
  const providerNone = providerStatus?.state === 'None';

  if (summary == null) {
    // No provider configured / reachable → "paste-your-own-summary" mode: a
    // summary from a third-party LLM can be pasted into the textarea and saved
    // as the session summary.
    const allowPaste = providerMissing || providerUnreachable || providerNone || providerUnsupported;
    if (allowPaste) {
      return (
        <div>
          <p className="meta" style={{ marginBottom: 8 }}>
            {providerNone
              ? (providerStatus?.hint ?? 'No AI provider selected.')
              : providerMissing
                ? (providerStatus?.hint ?? 'No summary provider configured.')
                : providerUnsupported
                  ? (providerStatus?.hint ?? 'This computer cannot run the built-in model.')
                  : (providerStatus?.hint ?? 'Summary provider is unreachable.')}{' '}
            Copy the transcript (Transcript tab → Copy) into any LLM, paste the
            result below, or configure a provider in{' '}
            {onNavigateToProviders
              ? <a data-testid="session-summary-providers" href="#" onClick={(e) => { e.preventDefault(); onNavigateToProviders(); }}>{providersPath()}</a>
              : providersPath()}.
          </p>
          <textarea
            data-testid="session-summary-paste"
            className="summary-edit"
            value={editText}
            onChange={(e) => setEditText(e.target.value)}
            placeholder="Paste a summary from any LLM here, or write your own."
            style={{ minHeight: 240 }}
          />
          <div className="summary-actions">
            <button
              data-testid="session-summary-paste-save"
              className="btn btn--primary"
              disabled={busy || !editText.trim()}
              onClick={onSaveEdit}
            >
              {busy ? 'Saving…' : 'Save summary'}
            </button>
            {err && <span data-testid="session-error" className="meta--danger">{err}</span>}
          </div>
        </div>
      );
    }
    return (
      <div style={{ textAlign: 'center', padding: '40px 0' }}>
        <p>No summary yet.</p>
        <button
          data-testid="session-summary-generate"
          className="btn btn--primary"
          disabled={busy || gated || summaryButtonsDisabled(providerStatus)}
          title={gated ? AI_BUSY_TITLE : undefined}
          onClick={onGenerate}
        >
          {busy ? 'Generating…' : 'Generate summary'}
        </button>
        {err && <p data-testid="session-error" className="meta--danger">{err}</p>}
        {providerLocked && (
          <p className="hint">
            The vault is locked — unlock the app to use summaries.
          </p>
        )}
        {!providerLocked && (
          <p className="meta" style={{ fontSize: 'var(--t-small)' }}>
            Generating runs transcription, dedup, then the AI summary — if this session hasn't been transcribed yet
            that step runs first and can take a minute or two (watch the progress overlay).
          </p>
        )}
      </div>
    );
  }

  if (editing) {
    return (
      <div>
        <MarkdownEditor value={editText} onChange={setEditText} autoFocus minHeight={280} />
        <div className="summary-actions">
          <button data-testid="session-summary-edit-save" className="btn btn--primary" disabled={busy} onClick={onSaveEdit}>{busy ? 'Saving…' : 'Save'}</button>
          <button data-testid="session-summary-edit-cancel" className="btn" onClick={() => setEditing(false)}>Cancel</button>
          {err && <span data-testid="session-error" className="meta--danger">{err}</span>}
        </div>
      </div>
    );
  }

  return (
    <div>
      {summary.tldr && (
        <div data-testid="session-summary-tldr" className="tldr-callout"><strong>TL;DR.</strong> {summary.tldr}</div>
      )}
      <div
        data-testid="session-summary-body"
        title="Click to edit the summary"
        style={{ cursor: 'text' }}
        onClick={() => { setEditText(summary.markdown); setEditing(true); }}
      >
        <MarkdownView markdown={summaryBody(summary.markdown)} />
      </div>
      {err && <p data-testid="session-error" className="meta--danger">{err}</p>}
    </div>
  );
}

async function exportSummaryMd(markdown: string, metaTitle: string): Promise<void> {
  const defaultFileName = `${metaTitle.replace(/[^\w.-]+/g, '_').slice(0, 60).replace(/_+$/, '') || 'meeting'}.md`;
  try {
    await tauri.saveTextFile(
      {
        title: 'Export meeting summary',
        defaultFileName,
        filterName: 'Markdown',
        extensions: ['md'],
      },
      markdown,
    );
  } catch (e: unknown) {
    console.error('export .md failed', e);
    void alert({
      title: "Couldn't save",
      body: `${(e as { message?: unknown })?.message ?? String(e)}`,
    });
  }
}

interface ActionMenuProps {
  hasSummary: boolean;
  busy: boolean;
  /** An AI task for another session or another kind holds the worker. */
  gated: boolean;
  providerStatus: { state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null;
  prompts: Prompt[];
  onEditSummary?: () => void;
  onRegenSummary?: (promptId?: string) => void | Promise<void>;
  onRegenTranscript: () => void | Promise<void>;
  onPolishTranscript: () => void | Promise<void>;
  onRepair: () => void | Promise<void>;
  onExportMd?: () => void;
  onDelete: () => void | Promise<void>;
  /** Absent when no chat thread is stored — the item is hidden. */
  onDeleteChat?: () => void | Promise<void>;
  sentIntegrationIds: string[];
  onSendTo: (integrationId: string) => void | Promise<void>;
}

function ActionMenu({ hasSummary, busy, gated, providerStatus, prompts, onEditSummary, onRegenSummary, onRegenTranscript, onPolishTranscript, onRepair, onExportMd, onDelete, onDeleteChat, sentIntegrationIds, onSendTo }: ActionMenuProps) {
  // Points at the transcript, where the cut point is chosen.
  async function howToCut(mode: 'split' | 'delete'): Promise<void> {
    await alert({
      title: mode === 'split' ? 'Split into a new meeting' : 'Delete from a point',
      body:
        'Open the Transcript tab, scroll to where the meeting should ' +
        (mode === 'split' ? 'split' : 'end') +
        ', then right-click that line and choose ' +
        (mode === 'split' ? '“Split into new meeting”' : '“Delete from here”') +
        '.\n\nThat line and everything before it is kept.',
    });
  }

  const [open, setOpen] = useState(false);
  const [sendOpen, setSendOpen] = useState(false);
  const [regenOpen, setRegenOpen] = useState(false);
  const [integrations, setIntegrations] = useState<IntegrationPublic[]>([]);
  const nerd = useNerdMode();
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) { setSendOpen(false); setRegenOpen(false); return; }
    tauri.listIntegrations()
      .then((all) => setIntegrations(all.filter((i) => i.enabled)))
      .catch(() => setIntegrations([]));
  }, [open]);
  useDismiss(open, rootRef, () => setOpen(false));

  function fire(fn?: () => void | Promise<void>) {
    setOpen(false);
    if (fn) void fn();
  }

  return (
    <div className="action-menu" ref={rootRef}>
      <button
        data-testid="session-actions"
        type="button"
        className="btn"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={busy}
        onClick={() => setOpen((v) => !v)}
      >
        <span>Action</span>
        <span className="action-menu__caret" aria-hidden="true">▾</span>
      </button>
      {open && (
        <div className="menu action-menu__pop" role="menu">
          <button
            data-testid="session-action-edit-summary"
            type="button"
            className="menu__item"
            role="menuitem"
            disabled={!hasSummary}
            onClick={() => fire(onEditSummary)}
          >
            ✎ Edit summary
          </button>
          <div
            className="action-menu__subwrap"
            onMouseEnter={() => setRegenOpen(true)}
            onMouseLeave={() => setRegenOpen(false)}
          >
            <button
              data-testid="session-action-regen-summary"
              type="button"
              className="menu__item"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={regenOpen}
              disabled={!hasSummary || gated || summaryButtonsDisabled(providerStatus)}
              title={gated ? AI_BUSY_TITLE : undefined}
              onClick={() => setRegenOpen((v) => !v)}
            >
              ↻ Regen. Summary <span className="action-menu__caret" aria-hidden="true">◂</span>
            </button>
            {regenOpen && hasSummary && !gated && !summaryButtonsDisabled(providerStatus) && (
              <div className="menu action-menu__pop action-menu__pop--sub" role="menu">
                {prompts.map((p) => (
                  <button
                    key={p.id}
                    data-testid={`session-action-regen-summary-${p.id}`}
                    type="button"
                    className="menu__item"
                    role="menuitem"
                    onClick={() => fire(() => onRegenSummary?.(p.id))}
                  >
                    {p.name}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            data-testid="session-action-regen-transcript"
            type="button"
            className="menu__item"
            role="menuitem"
            onClick={() => fire(onRegenTranscript)}
          >
            ↻ Regen. Transcript
          </button>
          {nerd && (
            <>
              <button
                data-testid="session-action-polish"
                type="button"
                className="menu__item"
                role="menuitem"
                disabled={summaryButtonsDisabled(providerStatus)}
                onClick={() => fire(onPolishTranscript)}
                title="Clean up punctuation/casing and redact secrets via the AI provider. Sends the transcript to your configured provider."
              >
                ✦ Polish Transcript
              </button>
              <button
                data-testid="session-action-repair"
                type="button"
                className="menu__item"
                role="menuitem"
                onClick={() => fire(onRepair)}
                title="Rebuild any missing files (transcript, audio, summary, chapters) without re-running what's already there."
              >
                ✚ Repair missing files
              </button>
            </>
          )}
          {nerd && (
            <>
              <div className="menu__sep" role="separator" />
              <button
                type="button"
                className="menu__item"
                role="menuitem"
                data-testid="session-action-split"
                onClick={() => fire(() => void howToCut('split'))}
                title="Move everything after a point in the transcript into its own recording."
              >
                ⤨ Split into new meeting
              </button>
              <button
                type="button"
                className="menu__item"
                role="menuitem"
                data-testid="session-action-delete-from"
                onClick={() => fire(() => void howToCut('delete'))}
                title="Trim a recording that was left running after the meeting ended."
              >
                ✂ Delete from a point…
              </button>
            </>
          )}
          <div className="menu__sep" role="separator" />
          <div
            className="action-menu__subwrap"
            onMouseEnter={() => setSendOpen(true)}
            onMouseLeave={() => setSendOpen(false)}
          >
            <button
              data-testid="session-action-send-to"
              type="button"
              className="menu__item"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={sendOpen}
              onClick={() => setSendOpen((v) => !v)}
            >
              ↗ Send To… <span className="action-menu__caret" aria-hidden="true">◂</span>
            </button>
            {sendOpen && (
              <div className="menu action-menu__pop action-menu__pop--sub" role="menu">
                {integrations.length === 0 ? (
                  <div data-testid="session-action-send-to-none" className="menu__item menu__item--note">
                    Define in Settings &gt; Integrations
                  </div>
                ) : integrations.map((i) => (
                  <button
                    key={i.id}
                    data-testid={`session-action-send-to-${i.id}`}
                    type="button"
                    className="menu__item"
                    role="menuitem"
                    onClick={() => fire(() => onSendTo(i.id))}
                  >
                    {i.name}
                    {sentIntegrationIds.includes(i.id) && (
                      <span className="meta--success" style={{ marginLeft: 6 }} aria-label="already sent">✓</span>
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            data-testid="session-action-export-md"
            type="button"
            className="menu__item"
            role="menuitem"
            disabled={!hasSummary}
            onClick={() => fire(onExportMd)}
          >
            ⇩ Export .md
          </button>
          <div className="menu__sep" role="separator" />
          <button
            data-testid="session-action-delete-recording"
            type="button"
            className="menu__item menu__item--danger"
            role="menuitem"
            onClick={() => fire(onDelete)}
          >
            ✕ Delete recording…
          </button>
          {onDeleteChat && (
            <button
              data-testid="session-action-delete-chat"
              type="button"
              className="menu__item menu__item--danger"
              role="menuitem"
              onClick={() => fire(onDeleteChat)}
            >
              ✕ Delete chat…
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// Initial tab on session open: unlabeled/low-confidence speakers →
// participants; no summary or no provider configured → transcript; else →
// summary.
function pickInitialTab(
  speakers: SessionSpeaker[],
  summary: SessionSummary | null,
  providerStatus: { state: SummaryProviderStatusKind },
): Tab {
  if (speakers.some(speakerNeedsReview)) return 'participants';
  const hasSummary = !!summary && summary.markdown.trim().length > 0;
  if (!hasSummary || providerStatus.state !== 'Configured') return 'transcript';
  return 'summary';
}

function ChaptersTOC({
  chapters, sessionId, onSeek, onChanged, disabled, hasTranscript, gated, jobRunning,
  providerStatus, onNavigateToProviders,
}: {
  chapters: SessionChapters | null;
  sessionId: string;
  onSeek?: (seconds: number) => void;
  onChanged: () => void | Promise<void>;
  disabled?: boolean;
  /** Chapters are cut from the transcript; there is nothing to run without one. */
  hasTranscript: boolean;
  /** An AI task for another session or another kind holds the worker. */
  gated?: boolean;
  /** The registry has an extraction running for this session. */
  jobRunning?: boolean;
  providerStatus: { state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null;
  onNavigateToProviders?: () => void;
}) {
  const [localBusy, setLocalBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Synchronous re-entrancy guard.
  const extractingRef = useRef(false);
  // Running state comes from the registry.
  const busy = localBusy || !!jobRunning;

  // Chapters run the same summary LLM as the Summary tab; the button is gated
  // the same way.
  const providerBlocked = summaryButtonsDisabled(providerStatus);

  async function extract() {
    if (extractingRef.current) return;
    extractingRef.current = true;
    setLocalBusy(true); setErr(null);
    try {
      const r = await tauri.extractSessionChapters({ session_id: sessionId });
      if (r.skipped) {
        setErr(r.reason ?? 'Chapter extraction was skipped — the transcript may be empty.');
      } else {
        await onChanged();
      }
    } catch (e) {
      setErr(errStr(e));
    } finally {
      extractingRef.current = false;
      setLocalBusy(false);
    }
  }

  const hasChapters = chapters && chapters.chapters.length > 0;

  return (
    <div className="panel-card" style={{ marginBottom: 16 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
        <strong className="side-group__title">
          Chapters {hasChapters && <span>({chapters.chapters.length})</span>}
        </strong>
        <button
          data-testid="session-chapters-extract"
          className="btn"
          disabled={busy || gated || disabled || providerBlocked || !hasTranscript}
          onClick={() => void extract()}
          title={gated
            ? AI_BUSY_TITLE
            : providerBlocked
              ? `Chapters need an AI provider. Set one up in ${providersPath()}.`
              : !hasTranscript
                ? 'Chapters are cut from the transcript. Transcribe this meeting first.'
                : 'Run the summary LLM over the transcript to identify topic chapters.'}
        >
          {busy ? 'Extracting…' : hasChapters ? 'Regenerate Chapters' : 'Extract chapters'}
        </button>
      </div>
      {providerBlocked && (
        <p className="meta" style={{ marginTop: 6, fontSize: 'var(--t-small)' }}>
          Chapters use your AI provider.{' '}
          {onNavigateToProviders
            ? <a data-testid="session-chapters-providers" href="#" onClick={(e) => { e.preventDefault(); onNavigateToProviders(); }}>Set one up in {providersPath()}</a>
            : `Set one up in ${providersPath()}`}{' '}
          to enable this.
        </p>
      )}
      {err && <p className="meta meta--danger" style={{ marginTop: 6, fontSize: 'var(--t-small)' }}>{err}</p>}
      {!hasChapters && !busy && !err && !providerBlocked && (
        <EmptyState testid="session-chapters-empty" title="No chapters yet" inset>
          Chapters mark where this meeting changed topic, each with a timestamp that jumps
          the transcript there.{' '}
          {hasTranscript
            ? 'Extract them from the transcript below.'
            : 'Transcribe this meeting to extract them.'}
        </EmptyState>
      )}
      {hasChapters && (
        <ol style={{ marginTop: 10, marginBottom: 0, paddingLeft: 0, listStyle: 'none' }}>
          {chapters.chapters.map((c: Chapter, i: number) => (
            <li key={`${c.start_hms}-${i}`} style={{ marginBottom: 4 }}>
              <button
                data-testid={`session-chapter-${i}`}
                className="chapter-row"
                onClick={() => onSeek?.(hmsToSeconds(c.start_hms))}
                disabled={!onSeek}
              >
                <code className="meta" style={{ fontSize: 'var(--t-small)', minWidth: 64 }}>{c.start_hms}</code>
                <span>
                  <strong>{c.title}</strong>
                  {c.summary && <div className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 2 }}>{c.summary}</div>}
                </span>
              </button>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

/** Unified copy button for the transcript + summary. Copies the raw text when
 *  an AI summarizer is configured; when none is, it prepends a paste-into-any-
 *  LLM prompt header. */
export function CopyButton({ kind, text }: { kind: CopyKind; text: string }) {
  const ai = useAiProviderStatus();
  const [state, setState] = useState<'idle' | 'copied' | 'truncated' | 'failed'>('idle');
  const includePrompt = !ai.configured;
  const onCopy = async () => {
    try {
      await copyWithPrompt(kind, text, includePrompt);
      setState(copyOutcomeForSize(text.length));
      setTimeout(() => setState('idle'), 2500);
    } catch {
      setState('failed');
      setTimeout(() => setState('idle'), 4000);
    }
  };
  return (
    <button
      data-testid={`session-copy-${kind}`}
      type="button"
      className="btn"
      disabled={!text}
      onClick={() => void onCopy()}
      title={includePrompt
        ? 'Copy with an AI prompt header (no summarizer configured — paste into ChatGPT, Claude, etc.)'
        : 'Copy to clipboard'}
    >
      {state === 'idle' && 'Copy'}
      {state === 'copied' && 'Copied ✓'}
      {state === 'truncated' && 'Copied (large — may truncate)'}
      {state === 'failed' && 'Copy failed'}
    </button>
  );
}

/** Shows the live transcript whenever there is no canonical transcript.md yet
 *  — while the recording still runs, while finalize builds it, and after an
 *  interrupted finalize. Prefers the in-memory live store and falls back to
 *  the backend copy when the store was cleared (e.g. the app restarted
 *  mid-finalize), then to `emptyState` when neither holds anything. Swaps to
 *  transcript.md once finalize writes it (parent reloads on library:changed →
 *  md non-null). */
function LiveTranscriptView({ sessionId, emptyState }: { sessionId: string; emptyState: React.ReactNode }) {
  const live = useSyncExternalStore(subscribeLiveTranscript, getLiveTranscriptState);
  // The stopped session's overlay names its lines; null until it answers,
  // and once speakers exist.
  const [names, setNames] = useState<UndiarizedNames | null>(null);
  useEffect(() => {
    let alive = true;
    Promise.resolve()
      .then(() => tauri.readLiveNames(sessionId))
      .then((n) => { if (alive) setNames(n); })
      .catch(() => { if (alive) setNames(null); });
    return () => { alive = false; };
  }, [sessionId]);
  const banner = (
    <div className="live-off-card live-off-card--banner" role="status">
      <p className="live-off-card__title">Live transcript — finalizing…</p>
      <p className="live-off-card__body">
        The cleaned, diarized version will replace this when ready.
      </p>
    </div>
  );

  // Primary: the in-memory store still holds this session's live transcript
  // (it survives navigation until the next recording starts).
  if (live.sessionId === sessionId) {
    const finals = live.finals.filter((e): e is LiveTurn => !isMarker(e));
    if (finals.length > 0) {
      return (
        <div className="transcript-tab">
          {banner}
          <LabelledLines lines={labelLines(finals, names)} />
        </div>
      );
    }
  }

  // Fallback: store was cleared (app restarted mid-finalize) — read the
  // backend's committed live copy.
  return <LiveTranscriptFallback sessionId={sessionId} names={names} banner={banner} emptyState={emptyState} />;
}

function LabelledLines({ lines, base = 0 }: { lines: LabelledLine[]; base?: number }) {
  return (
    <>
      {lines.map((l, i) => (
        <div key={i} data-testid={`live-turn-${base + i}`} className={`turn ${l.side}`}>
          {l.label && <span className="who">{l.label}</span>}
          <span className="turn-text">{l.text}</span>
        </div>
      ))}
    </>
  );
}

function LiveTranscriptFallback({ sessionId, names, banner, emptyState }: { sessionId: string; names: UndiarizedNames | null; banner: React.ReactNode; emptyState: React.ReactNode }) {
  type Row = { track: 'mic' | 'system'; start_ms: number; end_ms: number; text: string };
  const [segs, setSegs] = useState<Row[] | null>(null);
  useEffect(() => {
    let alive = true;
    tauri
      .readLiveTranscript(sessionId)
      .then((rows) => {
        if (!alive) return;
        setSegs(rows.map((r) => ({
          track: r.track === 'mic' ? 'mic' : 'system',
          start_ms: r.start_ms,
          end_ms: r.end_ms,
          text: r.text,
        })));
      })
      .catch(() => { if (alive) setSegs([]); });
    return () => { alive = false; };
  }, [sessionId]);

  const lines = useMemo(() => labelLines(segs ?? [], names), [segs, names]);

  if (segs == null) return null;
  if (segs.length === 0) return <>{emptyState}</>;
  return (
    <div className="transcript-tab">
      {banner}
      <LabelledLines lines={lines} />
    </div>
  );
}

export function TranscriptTab({ md, sessionId, onSeek, finalizing = false, focusQuery = null, focusSeekMs = null, nameToSide = {}, onTruncated }: { md: string | null; sessionId?: string; onSeek?: (seconds: number) => void; finalizing?: boolean; focusQuery?: string | null; focusSeekMs?: number | null; nameToSide?: Record<string, 'room' | 'remote'>; onTruncated?: () => void }) {
  // Turns + the search-result focus target (first turn containing any query
  // token). These hooks are declared before the md==null early-returns below;
  // hook order is identical on every render.
  //
  // While the transcript has no speakers, the stopped session's live names
  // (overlay files) label its lines; they drop once finalize's speakers
  // land and the transcript text changes.
  const [liveNames, setLiveNames] = useState<UndiarizedNames | null>(null);
  useEffect(() => {
    if (md == null || !sessionId) { setLiveNames(null); return; }
    let alive = true;
    Promise.resolve()
      .then(() => tauri.readLiveNames(sessionId))
      .then((n) => { if (alive) setLiveNames(n); })
      .catch(() => { if (alive) setLiveNames(null); });
    return () => { alive = false; };
  }, [sessionId, md]);
  const turns = useMemo(() => {
    if (md == null) return [];
    // Consecutive same-speaker segments read as one paragraph. The turn
    // carries the first timestamp (citation focus, the cut menu); each part
    // keeps its own for click-to-seek, a part without one inheriting the
    // previous part's.
    const merged: MergedTurn[] = [];
    const source: Turn[] = liveNames && liveNames.lines.length > 0 ? namedTurns(liveNames) : parseTranscript(md);
    for (const t of source) {
      const last = merged[merged.length - 1];
      if (last && t.side && last.side === t.side && last.who === t.who) {
        last.text = `${last.text} ${t.text}`.trim();
        if (!last.ts) last.ts = t.ts;
        last.parts.push({ ts: t.ts ?? last.parts[last.parts.length - 1].ts, text: t.text });
      } else {
        merged.push({ ...t, parts: [{ ts: t.ts, text: t.text }] });
      }
    }
    return merged;
  }, [md, liveNames]);
  const focusIdx = useMemo(() => {
    if (turns.length === 0) return -1;
    // A cited moment wins over a query match: the turn containing the
    // timestamp (last turn starting at or before it).
    if (focusSeekMs != null) {
      const secs = focusSeekMs / 1000;
      let idx = -1;
      for (let i = 0; i < turns.length; i++) {
        const ts = turns[i].ts;
        if (!ts) continue;
        if (hmsToSeconds(ts) <= secs) idx = i;
        else break;
      }
      if (idx === -1) idx = turns.findIndex((t) => !!t.ts);
      return idx;
    }
    if (!focusQuery) return -1;
    const toks = focusQuery.toLowerCase().replace(/['"]/g, ' ').split(/\s+/).filter(Boolean);
    if (toks.length === 0) return -1;
    return turns.findIndex((t) => {
      const lc = t.text.toLowerCase();
      return toks.some((tok) => lc.includes(tok));
    });
  }, [focusQuery, focusSeekMs, turns]);
  const focusRef = useRef<HTMLDivElement | null>(null);
  const [flashIdx, setFlashIdx] = useState(-1);
  // Right-click target for "Delete from here": viewport coords + the turn,
  // and where the turn ends — the next timestamped turn's start.
  const [ctx, setCtx] = useState<
    { x: number; y: number; ts: string; text: string; endTs: string | null } | null
  >(null);
  useEffect(() => {
    if (!ctx) return;
    const close = () => setCtx(null);
    window.addEventListener('click', close);
    window.addEventListener('keydown', close);
    window.addEventListener('resize', close);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', close);
      window.removeEventListener('resize', close);
    };
  }, [ctx]);

  async function cutHere(
    at: { ts: string; text: string; endTs: string | null },
    mode: 'split' | 'delete',
  ): Promise<void> {
    setCtx(null);
    if (!sessionId) return;
    if (!at.endTs) {
      pushToast({
        id: `cut-${sessionId}`,
        severity: 'error',
        title: mode === 'split' ? 'Could not split' : 'Could not trim',
        body: 'Nothing recorded after this turn.',
      });
      return;
    }
    const snippet = at.text.length > 90 ? `${at.text.slice(0, 90)}…` : at.text;
    const tail = 'The summary and chapters are deleted either way and need regenerating.';
    const ok = await confirm({
      title: mode === 'split'
        ? `Split into a new meeting at ${at.ts}?`
        : `Delete everything after ${at.ts}?`,
      body:
        `Keeps this turn — “${snippet}” — and everything before it.\n\n` +
        (mode === 'split'
          ? `Everything after it moves into a new recording titled “Split recording”, keeping its own date and time. Tags and title do not follow it. ${tail}`
          : `Everything after it is removed — recording, transcript and summary. ${tail} This cannot be undone.`),
      confirmLabel: mode === 'split' ? 'Split here' : 'Delete from here',
      danger: mode === 'delete',
    });
    if (!ok) return;
    const cutMs = hmsToSeconds(at.endTs) * 1000;
    try {
      if (mode === 'split') {
        const r = await tauri.splitSessionAt(sessionId, cutMs);
        pushToast({
          id: `cut-${sessionId}`,
          severity: 'done',
          title: 'Split into a new meeting',
          body: `${r.chunks_moved} chunk(s) moved to “Split recording”. Both need their summaries regenerated.`,
        });
      } else {
        const r = await tauri.truncateSessionAfter(sessionId, cutMs);
        pushToast({
          id: `cut-${sessionId}`,
          severity: 'done',
          title: `Trimmed to ${formatDurationSeconds(r.duration_seconds)}`,
          body: `${r.chunks_removed} chunk(s) removed. Regenerate the summary when you're ready.`,
        });
      }
      onTruncated?.();
    } catch (e) {
      pushToast({
        id: `cut-${sessionId}`,
        severity: 'error',
        title: mode === 'split' ? 'Could not split' : 'Could not trim',
        body: errStr(e),
      });
    }
  }

  useEffect(() => {
    if (focusIdx < 0) return;
    setFlashIdx(focusIdx);
    // Centers the turn on the next frame, once it is in the DOM.
    const raf = requestAnimationFrame(() => {
      focusRef.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
    });
    const clear = setTimeout(() => setFlashIdx(-1), 2400);
    return () => { cancelAnimationFrame(raf); clearTimeout(clear); };
  }, [focusIdx]);

  if (md == null) {
    // Reached only when neither the live store nor live_transcript.jsonl
    // holds anything for this session; `finalizing` picks its wording.
    const empty = finalizing ? (
      <div className="live-off-card" role="status">
        <p className="live-off-card__title">Transcribing…</p>
        <p className="live-off-card__body">This page updates on its own when the transcript is ready.</p>
      </div>
    ) : (
      <div className="live-off-card" role="status">
        <p className="live-off-card__title">No transcript yet</p>
        <p className="live-off-card__body">
          Use Summarize on the recording screen, or Generate summary above (it runs transcription first).
        </p>
      </div>
    );
    if (!sessionId) return empty;
    return <LiveTranscriptView sessionId={sessionId} emptyState={empty} />;
  }
  return (
    <div className="transcript-tab">
      {turns.map((t, i) => {
        const isFocus = i === focusIdx;
        const foundCls = flashIdx === i ? ' turn--found' : '';
        if (!t.side) {
          return (
            <div key={i} data-testid={`transcript-turn-${i}`} ref={isFocus ? focusRef : undefined} className={`turn${foundCls}`}>
              <span className="turn-text">{t.text}</span>
            </div>
          );
        }
        const clickable = onSeek && t.ts;
        const tint = speakerTint(t.who);
        return (
          <div
            key={i}
            data-testid={`transcript-turn-${i}`}
            ref={isFocus ? focusRef : undefined}
            className={`turn ${t.side}${tint ? ' turn--tinted' : ''}${foundCls}`}
            onContextMenu={sessionId && t.ts ? (e) => {
              e.preventDefault();
              const endTs = turns.slice(i + 1).find((n) => n.ts)?.ts ?? null;
              setCtx({ x: e.clientX, y: e.clientY, ts: t.ts!, text: t.text, endTs });
            } : undefined}
            style={{
              cursor: clickable ? 'pointer' : undefined,
              ...(tint ? { '--turn-tint': tint } : {}),
            } as React.CSSProperties}
            {...(clickable
              ? {
                role: 'button',
                title: `Jump to ${t.ts}`,
                // A click on a part seeks to that part's timestamp; anywhere
                // else on the turn, to the turn's.
                onClick: (e: React.MouseEvent) => {
                  const part = (e.target as HTMLElement).closest<HTMLElement>('[data-seek-ts]');
                  onSeek!(hmsToSeconds(part?.dataset.seekTs ?? t.ts!));
                },
              }
              : {})}
          >
            {t.side !== 'me' && (
              <span className="who">
                {t.who}
                {sideBadge(t.who, nameToSide) && (
                  <span className="who-side-badge">{sideBadge(t.who, nameToSide)}</span>
                )}
              </span>
            )}
            <span className="turn-text">
              {t.ts && <span className="turn-ts">{t.ts}</span>}
              {t.parts.map((p, k) => (
                <span
                  key={k}
                  data-seek-ts={p.ts ?? undefined}
                  title={clickable && p.ts ? `Jump to ${p.ts}` : undefined}
                >
                  {k > 0 ? ' ' : ''}{p.text}
                </span>
              ))}
            </span>
          </div>
        );
      })}
      {ctx && (
        <div
          data-testid="transcript-ctx"
          className="menu ctx-menu"
          style={{ left: ctx.x, top: ctx.y }}
          role="menu"
          onClick={(e) => e.stopPropagation()}
        >
          <button
            data-testid="transcript-ctx-split"
            type="button"
            role="menuitem"
            className="menu__item"
            onClick={() => void cutHere(ctx, 'split')}
          >
            Split into new meeting
          </button>
          <button
            data-testid="transcript-ctx-delete-from"
            type="button"
            role="menuitem"
            className="menu__item menu__item--danger"
            onClick={() => void cutHere(ctx, 'delete')}
          >
            Delete from here
          </button>
        </div>
      )}
    </div>
  );
}
