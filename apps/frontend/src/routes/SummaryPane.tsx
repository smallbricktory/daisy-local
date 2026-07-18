import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { save } from '@tauri-apps/plugin-dialog';
import {
  tauri,
  errStr,
  formatDurationHm,
  summaryProviderStatus,
  type Chapter,
  type IntegrationPublic,
  type SessionFocus,
  type SessionChapters,
  type SessionSpeaker,
  type SessionSummary,
  type SummaryProviderStatusKind,
  type Prompt,
  type Tag,
} from '../tauri';
import { TagChip } from '../components/tags/TagChip';
import { CallChat } from '../components/CallChat';
import { TagCombobox } from '../components/tags/TagCombobox';
import { TagPromptModal } from '../components/tags/TagPromptModal';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { MarkdownView } from '../components/MarkdownView';
import { MarkdownEditor } from '../components/MarkdownEditor';
import type { JobKind } from '../lib/sessionPhase';
import { copyOutcomeForSize } from '../lib/copy-state';
import { copyWithPrompt, type CopyKind } from '../lib/copyWithPrompt';
import { useAiProviderStatus } from '../lib/aiProviderStatus';
import { summaryButtonsDisabled } from '../lib/summary-gate';
import { useFinalizing } from '../lib/finalizeRunner';
import { confirm, alert } from '../lib/confirm';
import { SpeakerLabeler, speakerNeedsReview } from '../components/SpeakerLabeler';
import { useSessionData } from '../lib/useSessionData';
import { useSessionPhase } from '../lib/sessionLifecycle';
import {
  subscribeLiveTranscript,
  getLiveTranscriptState,
  isPauseMarker,
  type LiveTurn,
} from '../liveTranscript';

type Tab = 'summary' | 'notes' | 'transcript' | 'participants' | 'chat';

// Copy button style for inline placement on the detail-tabs bar: right-aligned,
// normal-case label.
const COPY_BTN_STYLE: React.CSSProperties = {
  marginLeft: 'auto', padding: '0 12px', fontSize: 13, fontWeight: 600,
  textTransform: 'none', letterSpacing: 'normal',
};

interface ManifestLoose {
  created_at_unix_seconds?: number;
  finalized_at_unix_seconds?: number;
  chunks?: { duration_seconds?: number | null }[];
  diarization_unavailable?: boolean;
}

const DATE_TIME_FMT = new Intl.DateTimeFormat(undefined, {
  month: 'short', day: 'numeric', year: 'numeric',
  hour: 'numeric', minute: '2-digit',
});
function fmtDateTime(unixSec: number | undefined): string {
  if (!unixSec) return '—';
  return DATE_TIME_FMT.format(new Date(unixSec * 1000));
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
  /** Navigate to Settings → Providers. Used by inline hints when no summary
   *  provider is configured. */
  onNavigateToProviders?: () => void;
  /** Deep-link target from a search result: which tab to open and (for the
   *  transcript) a query to scroll to + highlight. */
  focus?: SessionFocus;
}

export function SummaryPane({ sessionId, onMetaChanged, onStartSummarize, isProcessing = false, onDeleted, onNavigateToProviders, focus }: SummaryPaneProps) {
  const { view, meta, summary, chapters, speakers, tags, loadErr, reload } = useSessionData(sessionId);

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
  const [localBusy, setLocalBusy] = useState(false);
  // A background job (cascade / regen-summary / regen-transcript) for this
  // session also counts as busy. `useFinalizing` tracks the finalizeRunner's
  // in-flight set.
  const finalizing = useFinalizing(sessionId);
  const busy = localBusy || isProcessing || finalizing;
  const setBusy = setLocalBusy;
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
  const [audioError, setAudioError] = useState<string | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  const [providerStatus, setProviderStatus] = useState<{ state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null>(null);
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

  useEffect(() => {
    summaryProviderStatus().then(setProviderStatus).catch(() => setProviderStatus(null));
  }, []);

  // Loads meeting.opus via IPC bytes → Blob URL. The Blob URL is revoked on
  // session change / unmount.
  const loadPlaybackAudio = useCallback(
    async (sid: string): Promise<string | null> => {
      const exists = await tauri.sessionHasPlaybackAudio(sid).catch(() => false);
      if (!exists) return null;
      // The backend returns raw Ogg-Opus on Linux/Windows and an 8 kHz µ-law
      // WAV on macOS, where WebKit/CoreMedia mis-clocks Ogg-Opus (wrong
      // duration, skewed seek time-base). The magic bytes are sniffed to set
      // the Blob MIME.
      const bytes = await tauri.sessionPlaybackAudioBytes(sid);
      const sig = new Uint8Array(bytes.slice(0, 4));
      const isRiff = sig[0] === 0x52 && sig[1] === 0x49 && sig[2] === 0x46 && sig[3] === 0x46; // "RIFF"
      const type = isRiff ? 'audio/wav' : 'audio/ogg; codecs=opus';
      const blob = new Blob([bytes], { type });
      return URL.createObjectURL(blob);
    },
    [],
  );

  useEffect(() => {
    // New session — reset local UI only; useSessionData owns the data fetch.
    setUserPickedTab(false);
    setTab('summary');
    setEditing(false);
    setErr(null);
    setNotesText(null);
    setEditingTitle(false);
    setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return null; });
    setAudioError(null);
    let cancelled = false;
    loadPlaybackAudio(sessionId).then((url) => {
      if (cancelled) {
        if (url) URL.revokeObjectURL(url);
      } else {
        setAudioUrl(url);
      }
    }).catch(() => { /* keep null */ });
    return () => { cancelled = true; };
  }, [sessionId, loadPlaybackAudio]);

  // Reloads playback audio when the session's progress channel reports a stage
  // that rebuilds meeting.opus. Transcript/summary/speaker data reloads come
  // from library:changed via useSessionData, not from here.
  useEffect(() => {
    let un: UnlistenFn | undefined;
    const AUDIO_STAGES = new Set(['compressing', 'done']);
    listen<{ stage: string }>(`daisy://session/${sessionId}/progress`, (ev) => {
      if (AUDIO_STAGES.has(ev.payload.stage)) {
        loadPlaybackAudio(sessionId)
          .then((url) => { setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return url; }); })
          .catch(() => { /* keep current url */ });
      }
    }).then((u) => { un = u; });
    return () => { if (un) un(); };
  }, [sessionId, loadPlaybackAudio]);

  // Revokes any outstanding Blob URL on unmount.
  useEffect(() => () => {
    setAudioUrl((prev) => { if (prev) URL.revokeObjectURL(prev); return null; });
  }, []);

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

  if (loadErr) return <div className="summary-pane"><p style={{ color: 'var(--danger)' }}>Failed to load: {loadErr}</p></div>;
  if (!view || !meta) return <div className="summary-pane"><p style={{ color: 'var(--iron)' }}>Loading…</p></div>;

  const manifest = (view.manifest_json ?? null) as ManifestLoose | null;
  const importedTag = sessionId.startsWith('daisy-import-') ? ' · Imported' : '';
  const eyebrowTail = summary
    ? `${fmtDurationHm(manifest)}${importedTag}`
    : `${fmtDurationHm(manifest)} · not summarized yet${importedTag}`;
  const others = meta.attendees.filter((a) => a.role === 'other').map((a) => a.display_name);

  // Separate date + time inputs, committed only by an explicit Save.
  // datetime-local renders without a usable picker/time field in WebKitGTK
  // and WKWebView.
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
              type="date"
              autoFocus
              value={editDateVal}
              onChange={(e) => setEditDateVal(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Escape') setEditingDate(false); if (e.key === 'Enter') void saveDate(); }}
              style={{ colorScheme: 'light' }}
            />
            <input
              type="time"
              value={editTimeVal}
              onChange={(e) => setEditTimeVal(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Escape') setEditingDate(false); if (e.key === 'Enter') void saveDate(); }}
              style={{ colorScheme: 'light' }}
            />
            <button className="btn btn--primary" onClick={() => void saveDate()}>Save</button>
            <button className="btn" onClick={() => setEditingDate(false)}>Cancel</button>
          </span>
        ) : (
          <span
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
            className="summary-pane__title"
            autoFocus
            defaultValue={meta.title ?? ''}
            style={{ display: 'block', flex: 1, height: 'auto', padding: 0, borderRadius: 0, background: 'transparent', border: 'none', borderBottom: '2px solid var(--indigo)', outline: 'none' }}
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
          <h1 className="summary-pane__title" style={{ cursor: 'text', flex: 1, margin: 0 }} title="Click to rename" onClick={() => setEditingTitle(true)}>
            {meta.title || fmtDateTime(manifest?.created_at_unix_seconds)}
          </h1>
        )}
        <ActionMenu
          hasSummary={summary != null}
          busy={busy}
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
        <TagCombobox
          excludeIds={meta.tag_ids}
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


      {audioUrl && (
        <div className="playback">
          <audio
            ref={audioRef}
            controls
            // With `metadata`, WebKitGTK leaves the buffer in a half-loaded
            // state — the scrubber renders a spinner until the user manually
            // seeks. `auto` forces a full decode pass; duration + buffered
            // ranges resolve immediately.
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
          />
          {audioError && (
            <span className="playback__err" title={audioError}>
              can't play — {audioError}
            </span>
          )}
        </div>
      )}

      <div className="detail-tabs">
        {(['summary', 'notes', 'transcript', 'participants', 'chat'] as Tab[]).map((t) => (
          <span key={t} className={`detail-tabs__tab ${tab === t ? 'detail-tabs__tab--on' : ''}`} onClick={() => pickTab(t)}>
            {t}
          </span>
        ))}
        {/* Copy lives on the tab-bar line, contextual to the active tab. */}
        {tab === 'summary' && summary && (
          <CopyButton kind="summary" text={summary.markdown} label="Copy" style={COPY_BTN_STYLE} />
        )}
        {tab === 'transcript' && view.transcript_md && (
          <CopyButton kind="transcript" text={view.transcript_md} label="Copy all" style={COPY_BTN_STYLE} />
        )}
      </div>

      {tab === 'summary' && (
        <SummaryTab
          summary={summary}
          editing={editing}
          editText={editText}
          busy={busy}
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
          <MarkdownEditor value={notesText ?? ''} onChange={setNotesText} minHeight={260} />
          <div className="summary-actions">
            <button className="btn btn--primary" onClick={() => { tauri.sessionNotesSave(sessionId, notesText ?? '').catch((e) => setErr(errStr(e))); }}>Save notes</button>
            {err && <span style={{ color: 'var(--danger)' }}>{err}</span>}
          </div>
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
            providerStatus={providerStatus}
            onNavigateToProviders={onNavigateToProviders}
          />
          <TranscriptTab
            md={view.transcript_md}
            sessionId={sessionId}
            onSeek={audioUrl ? seek : undefined}
            finalizing={sessionFinalizing}
            focusQuery={focusQuery}
            focusSeekMs={focusSeekMs}
            nameToSide={Object.fromEntries((speakers ?? []).map((s) => [s.display_name, s.side]))}
          />
        </>
      )}

      {tab === 'participants' && (
        <SpeakerLabeler
          sessionId={sessionId}
          sessionTitle={meta?.title ?? null}
          diarizationUnavailable={!!manifest?.diarization_unavailable}
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
      <p className="summary-pane__sid">
        <em>{sessionId}</em>
      </p>
    </div>
  );
}

function SummaryTab(props: {
  summary: SessionSummary | null;
  editing: boolean;
  editText: string;
  busy: boolean;
  err: string | null;
  providerStatus: { state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null;
  onNavigateToProviders?: () => void;
  setEditing: (b: boolean) => void;
  setEditText: (s: string) => void;
  onGenerate: () => void;
  onSaveEdit: () => void;
}) {
  const { summary, editing, editText, busy, err, providerStatus, onNavigateToProviders, setEditing, setEditText, onGenerate, onSaveEdit } = props;

  const providerMissing = providerStatus?.state === 'Missing';
  const providerUnreachable = providerStatus?.state === 'Unreachable';
  const providerLocked = providerStatus?.state === 'VaultLocked';
  const providerNone = providerStatus?.state === 'None';

  if (summary == null) {
    // No provider configured / reachable → "paste-your-own-summary" mode: a
    // summary from a third-party LLM can be pasted into the textarea and saved
    // as the session summary.
    const allowPaste = providerMissing || providerUnreachable || providerNone;
    if (allowPaste) {
      return (
        <div>
          <p style={{ color: 'var(--iron)', fontSize: 13, marginBottom: 8 }}>
            {providerNone
              ? (providerStatus?.hint ?? 'No AI provider selected.')
              : providerMissing
                ? (providerStatus?.hint ?? 'No summary provider configured.')
                : (providerStatus?.hint ?? 'Summary provider is unreachable.')}{' '}
            Copy the transcript (Transcript tab → Copy all) into any LLM, paste the
            result below, or configure a provider in{' '}
            {onNavigateToProviders
              ? <a href="#" onClick={(e) => { e.preventDefault(); onNavigateToProviders(); }}>Settings → Providers</a>
              : 'Settings → Providers'}.
          </p>
          <textarea
            className="summary-edit"
            value={editText}
            onChange={(e) => setEditText(e.target.value)}
            placeholder="Paste a summary from any LLM here, or write your own."
            style={{ minHeight: 240 }}
          />
          <div className="summary-actions">
            <button
              className="btn btn--primary"
              disabled={busy || !editText.trim()}
              onClick={onSaveEdit}
            >
              {busy ? 'Saving…' : 'Save summary'}
            </button>
            {err && <span style={{ color: 'var(--danger)' }}>{err}</span>}
          </div>
        </div>
      );
    }
    return (
      <div style={{ textAlign: 'center', padding: '40px 0' }}>
        <p>No summary yet.</p>
        <button
          className="btn btn--primary"
          disabled={busy || summaryButtonsDisabled(providerStatus)}
          onClick={onGenerate}
        >
          {busy ? 'Generating…' : 'Generate summary'}
        </button>
        {err && <p style={{ color: 'var(--danger)' }}>{err}</p>}
        {providerLocked && (
          <p className="hint" style={{ color: 'var(--iron)', fontSize: 12 }}>
            The vault is locked — unlock the app to use summaries.
          </p>
        )}
        {!providerLocked && (
          <p style={{ color: 'var(--iron)', fontSize: 12 }}>
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
          <button className="btn btn--primary" disabled={busy} onClick={onSaveEdit}>{busy ? 'Saving…' : 'Save'}</button>
          <button className="btn" onClick={() => setEditing(false)}>Cancel</button>
          {err && <span style={{ color: 'var(--danger)' }}>{err}</span>}
        </div>
      </div>
    );
  }

  return (
    <div>
      <div className="tldr-callout"><strong>TL;DR.</strong> {summary.structured.tldr}</div>
      <div
        title="Click to edit the summary"
        style={{ cursor: 'text' }}
        onClick={() => { setEditText(summary.markdown); setEditing(true); }}
      >
        <MarkdownView markdown={summaryBody(summary.markdown)} />
      </div>
      {err && <p style={{ color: 'var(--danger)' }}>{err}</p>}
    </div>
  );
}

async function exportSummaryMd(markdown: string, metaTitle: string): Promise<void> {
  const defaultFileName = `${metaTitle.replace(/[^\w.-]+/g, '_') || 'meeting'}.md`;
  let dest: string | null = null;
  try {
    dest = await save({
      title: 'Export meeting summary',
      defaultPath: defaultFileName,
      filters: [{ name: 'Markdown', extensions: ['md'] }],
    });
  } catch {
    return;
  }
  if (!dest) return;
  try {
    await tauri.saveTextFile(dest, markdown);
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

function ActionMenu({ hasSummary, busy, providerStatus, prompts, onEditSummary, onRegenSummary, onRegenTranscript, onPolishTranscript, onRepair, onExportMd, onDelete, onDeleteChat, sentIntegrationIds, onSendTo }: ActionMenuProps) {
  const [open, setOpen] = useState(false);
  const [sendOpen, setSendOpen] = useState(false);
  const [regenOpen, setRegenOpen] = useState(false);
  const [integrations, setIntegrations] = useState<IntegrationPublic[]>([]);
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) { setSendOpen(false); setRegenOpen(false); return; }
    tauri.listIntegrations()
      .then((all) => setIntegrations(all.filter((i) => i.enabled)))
      .catch(() => setIntegrations([]));
    function onDocClick(e: MouseEvent) {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) { if (e.key === 'Escape') setOpen(false); }
    document.addEventListener('mousedown', onDocClick);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDocClick);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  function fire(fn?: () => void | Promise<void>) {
    setOpen(false);
    if (fn) void fn();
  }

  return (
    <div className="action-menu" ref={rootRef}>
      <button
        type="button"
        className="action-menu__btn"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={busy}
        onClick={() => setOpen((v) => !v)}
      >
        <span>Action</span>
        <span className="action-menu__caret" aria-hidden="true">▾</span>
      </button>
      {open && (
        <div className="action-menu__pop" role="menu">
          <button
            type="button"
            className="action-menu__item"
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
              type="button"
              className="action-menu__item"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={regenOpen}
              disabled={!hasSummary || summaryButtonsDisabled(providerStatus)}
              onClick={() => setRegenOpen((v) => !v)}
            >
              ↻ Regen. Summary <span className="action-menu__caret" aria-hidden="true">◂</span>
            </button>
            {regenOpen && hasSummary && !summaryButtonsDisabled(providerStatus) && (
              <div className="action-menu__pop action-menu__pop--sub" role="menu">
                {prompts.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    className="action-menu__item"
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
            type="button"
            className="action-menu__item"
            role="menuitem"
            onClick={() => fire(onRegenTranscript)}
          >
            ↻ Regen. Transcript
          </button>
          <button
            type="button"
            className="action-menu__item"
            role="menuitem"
            disabled={summaryButtonsDisabled(providerStatus)}
            onClick={() => fire(onPolishTranscript)}
            title="Clean up punctuation/casing and redact secrets via the AI provider. Sends the transcript to your configured provider."
          >
            ✨ Polish Transcript
          </button>
          <button
            type="button"
            className="action-menu__item"
            role="menuitem"
            onClick={() => fire(onRepair)}
            title="Rebuild any missing files (transcript, audio, summary, chapters) without re-running what's already there."
          >
            🩺 Repair missing files
          </button>
          <div className="action-menu__sep" role="separator" />
          <div
            className="action-menu__subwrap"
            onMouseEnter={() => setSendOpen(true)}
            onMouseLeave={() => setSendOpen(false)}
          >
            <button
              type="button"
              className="action-menu__item"
              role="menuitem"
              aria-haspopup="menu"
              aria-expanded={sendOpen}
              onClick={() => setSendOpen((v) => !v)}
            >
              ↗ Send To… <span className="action-menu__caret" aria-hidden="true">◂</span>
            </button>
            {sendOpen && (
              <div className="action-menu__pop action-menu__pop--sub" role="menu">
                {integrations.length === 0 ? (
                  <div className="action-menu__item" aria-disabled="true" style={{ color: 'var(--iron)', cursor: 'default' }}>
                    Define in Settings &gt; Integrations
                  </div>
                ) : integrations.map((i) => (
                  <button
                    key={i.id}
                    type="button"
                    className="action-menu__item"
                    role="menuitem"
                    onClick={() => fire(() => onSendTo(i.id))}
                  >
                    {i.name}
                    {sentIntegrationIds.includes(i.id) && (
                      <span style={{ color: 'var(--success, #2e7d32)', marginLeft: 6 }} aria-label="already sent">✓</span>
                    )}
                  </button>
                ))}
              </div>
            )}
          </div>
          <button
            type="button"
            className="action-menu__item"
            role="menuitem"
            disabled={!hasSummary}
            onClick={() => fire(onExportMd)}
          >
            ⇩ Export .md
          </button>
          <div className="action-menu__sep" role="separator" />
          <button
            type="button"
            className="action-menu__item"
            role="menuitem"
            onClick={() => fire(onDelete)}
            style={{ color: 'var(--danger)' }}
          >
            🗑 Delete recording…
          </button>
          {onDeleteChat && (
            <button
              type="button"
              className="action-menu__item"
              role="menuitem"
              onClick={() => fire(onDeleteChat)}
              style={{ color: 'var(--danger)' }}
            >
              🗑 Delete chat…
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
  chapters, sessionId, onSeek, onChanged, disabled, providerStatus, onNavigateToProviders,
}: {
  chapters: SessionChapters | null;
  sessionId: string;
  onSeek?: (seconds: number) => void;
  onChanged: () => void | Promise<void>;
  disabled?: boolean;
  providerStatus: { state: SummaryProviderStatusKind; provider: string | null; hint: string | null } | null;
  onNavigateToProviders?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // Chapters run the same summary LLM as the Summary tab; the button is gated
  // the same way.
  const providerBlocked = summaryButtonsDisabled(providerStatus);

  async function extract() {
    setBusy(true); setErr(null);
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
      setBusy(false);
    }
  }

  const hasChapters = chapters && chapters.chapters.length > 0;

  return (
    <div style={{
      marginBottom: 16, padding: '10px 14px',
      border: '1px solid var(--frost-deep)', borderRadius: 8,
      background: 'var(--cream-pure)',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
        <strong style={{ fontSize: 13, letterSpacing: '0.08em', textTransform: 'uppercase', color: 'var(--iron)' }}>
          Chapters {hasChapters && <span style={{ color: 'var(--muted)' }}>({chapters.chapters.length})</span>}
        </strong>
        <button
          className="btn"
          disabled={busy || disabled || providerBlocked}
          onClick={() => void extract()}
          title={providerBlocked
            ? 'Chapters need an AI provider. Set one up in Settings → Providers.'
            : 'Run the summary LLM over the transcript to identify topic chapters.'}
        >
          {busy ? 'Extracting…' : hasChapters ? 'Regenerate Chapters' : 'Extract chapters'}
        </button>
      </div>
      {providerBlocked && (
        <p className="meta" style={{ marginTop: 6, fontSize: 12 }}>
          Chapters use your AI provider.{' '}
          {onNavigateToProviders
            ? <a href="#" onClick={(e) => { e.preventDefault(); onNavigateToProviders(); }}>Set one up in Settings → Providers</a>
            : 'Set one up in Settings → Providers'}{' '}
          to enable this.
        </p>
      )}
      {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 6, fontSize: 12 }}>{err}</p>}
      {hasChapters && (
        <ol style={{ marginTop: 10, marginBottom: 0, paddingLeft: 0, listStyle: 'none' }}>
          {chapters.chapters.map((c: Chapter, i: number) => (
            <li key={`${c.start_hms}-${i}`} style={{ marginBottom: 4 }}>
              <button
                className="chapter-row"
                onClick={() => onSeek?.(hmsToSeconds(c.start_hms))}
                disabled={!onSeek}
                style={{
                  display: 'flex', gap: 10, alignItems: 'baseline',
                  width: '100%', textAlign: 'left',
                  padding: '4px 6px', borderRadius: 4,
                  border: '1px solid transparent',
                  background: 'transparent',
                  cursor: onSeek ? 'pointer' : 'default',
                  fontFamily: 'inherit', fontSize: 14,
                }}
                onMouseEnter={(e) => { if (onSeek) (e.currentTarget as HTMLButtonElement).style.background = 'var(--tint)'; }}
                onMouseLeave={(e) => { (e.currentTarget as HTMLButtonElement).style.background = 'transparent'; }}
              >
                <code style={{ fontSize: 12, color: 'var(--iron)', minWidth: 64 }}>{c.start_hms}</code>
                <span>
                  <strong>{c.title}</strong>
                  {c.summary && <div className="meta" style={{ fontSize: 12, marginTop: 2 }}>{c.summary}</div>}
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
export function CopyButton({ kind, text, label, style }: { kind: CopyKind; text: string; label: string; style?: React.CSSProperties }) {
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
      type="button"
      className="btn"
      style={style}
      disabled={!text}
      onClick={() => void onCopy()}
      title={includePrompt
        ? 'Copy with an AI prompt header (no summarizer configured — paste into ChatGPT, Claude, etc.)'
        : 'Copy to clipboard'}
    >
      {state === 'idle' && label}
      {state === 'copied' && 'Copied ✓'}
      {state === 'truncated' && 'Copied (large — may truncate)'}
      {state === 'failed' && 'Copy failed'}
    </button>
  );
}

/** Shows the live transcript while finalize builds the canonical one. Prefers
 *  the in-memory live store and falls back to the backend copy when the store
 *  was cleared (e.g. the app restarted mid-finalize). Swaps to transcript.md
 *  once finalize writes it (parent reloads on library:changed → md non-null). */
function LiveTranscriptView({ sessionId }: { sessionId: string }) {
  const live = useSyncExternalStore(subscribeLiveTranscript, getLiveTranscriptState);
  const banner = (
    <p style={{ color: 'var(--iron)', fontStyle: 'italic', marginBottom: 8 }}>
      Live transcript — finalizing… (the cleaned, diarized version will replace this when ready)
    </p>
  );

  // Primary: the in-memory store still holds this session's live transcript
  // (it survives navigation until the next recording starts).
  if (live.sessionId === sessionId) {
    const finals = live.finals.filter((e): e is LiveTurn => !isPauseMarker(e));
    if (finals.length > 0) {
      return (
        <div className="transcript-tab">
          {banner}
          {finals.map((t, i) => (
            <div key={i} className={`turn ${t.track === 'mic' ? 'me' : 'them'}`}>
              {t.track !== 'mic' && <span className="who">Them</span>}
              <span className="turn-text">{t.text}</span>
            </div>
          ))}
        </div>
      );
    }
  }

  // Fallback: store was cleared (app restarted mid-finalize) — read the
  // backend's committed live copy.
  return <LiveTranscriptFallback sessionId={sessionId} banner={banner} />;
}

function LiveTranscriptFallback({ sessionId, banner }: { sessionId: string; banner: React.ReactNode }) {
  const [segs, setSegs] = useState<{ track: string; text: string }[] | null>(null);
  useEffect(() => {
    let alive = true;
    tauri
      .readLiveTranscript(sessionId)
      .then((rows) => {
        if (!alive) return;
        // Same-track rows coalesce into paragraphs, matching the live view.
        const grouped: { track: string; text: string }[] = [];
        for (const r of rows) {
          const last = grouped[grouped.length - 1];
          if (last && last.track === r.track) last.text = `${last.text} ${r.text}`.trim();
          else grouped.push({ track: r.track, text: r.text });
        }
        setSegs(grouped);
      })
      .catch(() => { if (alive) setSegs([]); });
    return () => { alive = false; };
  }, [sessionId]);

  if (segs == null || segs.length === 0) {
    return (
      <p style={{ color: 'var(--iron)' }}>
        Transcribing… this page updates on its own when the transcript is ready.
      </p>
    );
  }
  return (
    <div className="transcript-tab">
      {banner}
      {segs.map((s, i) => (
        <div key={i} className={`turn ${s.track === 'mic' ? 'me' : 'them'}`}>
          {s.track !== 'mic' && <span className="who">Them</span>}
          <span className="turn-text">{s.text}</span>
        </div>
      ))}
    </div>
  );
}

export function TranscriptTab({ md, sessionId, onSeek, finalizing = false, focusQuery = null, focusSeekMs = null, nameToSide = {} }: { md: string | null; sessionId?: string; onSeek?: (seconds: number) => void; finalizing?: boolean; focusQuery?: string | null; focusSeekMs?: number | null; nameToSide?: Record<string, 'room' | 'remote'> }) {
  // Turns + the search-result focus target (first turn containing any query
  // token). These hooks are declared before the md==null early-returns below;
  // hook order is identical on every render.
  const turns = useMemo(() => {
    if (md == null) return [];
    // Consecutive same-speaker segments read as one paragraph; the first
    // timestamp survives for click-to-seek and citation focus.
    const merged: Turn[] = [];
    for (const t of parseTranscript(md)) {
      const last = merged[merged.length - 1];
      if (last && t.side && last.side === t.side && last.who === t.who) {
        last.text = `${last.text} ${t.text}`.trim();
        if (!last.ts) last.ts = t.ts;
      } else {
        merged.push({ ...t });
      }
    }
    return merged;
  }, [md]);
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
    if (finalizing) {
      // Shows the live transcript until the canonical transcript.md is on disk.
      if (sessionId) {
        return <LiveTranscriptView sessionId={sessionId} />;
      }
      return (
        <p style={{ color: 'var(--iron)' }}>
          Transcribing… this page updates on its own when the transcript is ready.
        </p>
      );
    }
    return (
      <p style={{ color: 'var(--iron)' }}>
        No transcript yet. Use Summarize on the recording screen, or Generate summary above (it runs transcription first).
      </p>
    );
  }
  return (
    <div className="transcript-tab">
      {turns.map((t, i) => {
        const isFocus = i === focusIdx;
        const foundCls = flashIdx === i ? ' turn--found' : '';
        if (!t.side) {
          return (
            <div key={i} ref={isFocus ? focusRef : undefined} className={`turn${foundCls}`}>
              <span className="turn-text">{t.text}</span>
            </div>
          );
        }
        const clickable = onSeek && t.ts;
        const tint = speakerTint(t.who);
        return (
          <div
            key={i}
            ref={isFocus ? focusRef : undefined}
            className={`turn ${t.side}${foundCls}`}
            style={{
              cursor: clickable ? 'pointer' : undefined,
              background: tint ?? undefined,
            }}
            {...(clickable
              ? { role: 'button', title: `Jump to ${t.ts}`, onClick: () => onSeek!(hmsToSeconds(t.ts!)) }
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
              {t.text}
            </span>
          </div>
        );
      })}
    </div>
  );
}
