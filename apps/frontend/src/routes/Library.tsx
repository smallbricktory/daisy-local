// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { tauri, formatDurationSeconds, errStr, type LicenseStatus, type SessionFocus, type SessionListEntry, type Tag } from '../tauri';
import { expiredBannerState } from '../lib/expired-banner';
import { SummaryPane } from './SummaryPane';
import type { JobKind } from '../lib/sessionPhase';
import { subscribeToLibrary } from '../lib/libraryEvents';
import { useTags } from '../lib/useTags';
import { useTagSuggestions } from '../lib/useTagSuggestions';
import { fmtDate, fmtTime } from '../lib/format';
import { recordingKindLabel } from '../lib/recordingKind';
import { TagCombobox } from '../components/tags/TagCombobox';
import { GhostSuggestionChips } from '../components/tags/SuggestedTags';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';

interface Props {
  selectedSessionId?: string;
  /** The session currently being recorded, excluded from auto-selection. */
  recordingSessionId?: string;
  /** Optional deep-link target (tab + transcript query) from a search result. */
  focus?: SessionFocus;
  onSelect: (id: string) => void;
  /** Navigate to the recorder — the step an empty library offers. */
  onStartRecording: () => void;
  /** Forwarded to SummaryPane; passed up to App, which runs the cascade in
   *  the background. */
  onStartSummarize: (sessionId: string, title: string, kind?: JobKind, promptId?: string) => void;
  /** Session IDs with a background job running. SummaryPane disables the
   *  Generate button for a session whose cascade is still running. */
  processingSessionIds: string[];
  /** Navigate to the AI provider picker. Forwarded to SummaryPane for inline hints. */
  onNavigateToProviders?: () => void;
  /** Disables "+ Meeting" (which can import audio) once expired. */
  license?: LicenseStatus | null;
}

function sortByNewest(rows: SessionListEntry[]): SessionListEntry[] {
  return [...rows].sort((a, b) => b.created_at_unix_seconds - a.created_at_unix_seconds);
}

/** Dots a row draws before it collapses to a count. Two colours read as a
 *  cluster; more read as a bar. */
const TAG_DOTS = 2;

function RowGhosts({ s }: { s: SessionListEntry }) {
  const suggestions = useTagSuggestions(
    s.tag_ids.length === 0 && s.has_transcript ? s.session_id : null,
  );
  const tags = useTags();
  if (!suggestions || suggestions.vocab.length === 0) return null;
  return (
    <span data-testid={`library-ghosts-${s.session_id}`} onClick={(e) => e.stopPropagation()}>
      <GhostSuggestionChips
        suggestions={suggestions.vocab}
        tags={tags}
        onAccept={(tagId) =>
          void tauri.sessionAssignTags(s.session_id, [tagId])
        }
      />
    </span>
  );
}

// Last fetched list, module-scoped: a remount (e.g. recording → library nav)
// renders the previous list immediately instead of flashing the empty state
// while the refetch is in flight.
let cachedSessions: SessionListEntry[] = [];

export function Library({ selectedSessionId, recordingSessionId, focus, onSelect, onStartRecording, onStartSummarize, processingSessionIds, onNavigateToProviders, license }: Props) {
  const expiredBanner = expiredBannerState(license ?? null);
  const [sessions, setSessionsState] = useState<SessionListEntry[]>(cachedSessions);
  const tags = useTags();
  /** The tag list by id — every row reads a few of them, so the list is walked
   *  once rather than once per dot. */
  const tagById = useMemo(() => new Map(tags.map((t) => [t.id, t])), [tags]);
  /** Names behind a row's collapsed dot cluster, for its tooltip. */
  const tagNames = (ids: string[]) => ids.map((id) => tagById.get(id)?.name).filter((n): n is string => !!n);
  const setSessions = (rows: SessionListEntry[]) => { cachedSessions = rows; setSessionsState(rows); };
  const [filterTagIds, setFilterTagIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Draggable width of the session list (px), persisted.
  const libRef = useRef<HTMLDivElement>(null);
  // null = never dragged, so the stylesheet's default width stands. The number
  // lives in one place that way, where a rule can read it.
  const [listWidth, setListWidth] = useState<number | null>(() => {
    const v = Number(localStorage.getItem('daisy.library.listWidth'));
    return Number.isFinite(v) && v >= 180 && v <= 700 ? v : null;
  });
  function startListDrag(e: React.MouseEvent) {
    e.preventDefault();
    const move = (ev: MouseEvent) => {
      const left = libRef.current?.getBoundingClientRect().left ?? 0;
      const w = Math.max(180, Math.min(700, ev.clientX - left));
      setListWidth(w);
    };
    const up = () => {
      document.removeEventListener('mousemove', move);
      document.removeEventListener('mouseup', up);
      if (listWidthRef.current !== null) localStorage.setItem('daisy.library.listWidth', String(listWidthRef.current));
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }
  const listWidthRef = useRef(listWidth);
  listWidthRef.current = listWidth;

  const reload = useCallback(async () => {
    try {
      const rows = await tauri.listSessions();
      setSessions(sortByNewest(rows));
      setError(null);
    } catch (e) {
      setError(String((e as { message?: unknown })?.message ?? e));
    }
  }, []);

  useEffect(() => { reload(); }, [reload]);

  // Backend emits `library:changed` on every mutation; refetch on each
  // event. The libraryEvents bus also fans out a synthetic event on
  // visibilitychange-resume, catching up on anything fired while the window
  // was hidden.
  useEffect(() => {
    return subscribeToLibrary(() => {
      tauri.listSessions()
        .then((rows) => setSessions(sortByNewest(rows)))
        .catch(() => { /* ignored */ });
    });
  }, []);

  // Only show tags in the filter bar that are actually attached to something.
  const usedTagIds = new Set(sessions.flatMap((s) => s.tag_ids));
  const filterTags = tags.filter((t) => usedTagIds.has(t.id));

  // Intersection: each selected tag narrows the list.
  const visible = filterTagIds.length === 0
    ? sessions
    : sessions.filter((s) => filterTagIds.every((id) => s.tag_ids.includes(id)));

  const inList = selectedSessionId && visible.some((s) => s.session_id === selectedSessionId);
  // The auto-selected fallback must never be the session being recorded: its
  // detail pane has no transport controls — that session opens on the
  // recording screen (via onSelect) when clicked explicitly.
  const effectiveSelected = inList
    ? selectedSessionId
    : visible.find((s) => s.session_id !== recordingSessionId)?.session_id;

  const [noteOpen, setNoteOpen] = useState(false);

  function toggleFilter(id: string) {
    setFilterTagIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  return (
    <div className="page page--bleed">
      <PageHeader
        title="Library"
        actions={
          <button
            data-testid="library-new-meeting"
            className="btn"
            onClick={() => setNoteOpen(true)}
            disabled={expiredBanner.kind === 'show'}
            title={expiredBanner.kind === 'show'
              ? expiredBanner.label
              : 'Add a meeting from notes and/or an audio file you upload (iPhone, another app…).'}
          >+ Meeting</button>
        }
        filters={
          filterTags.length > 0 ? (
            <>
              {filterTags.map((t) => {
                const on = filterTagIds.includes(t.id);
                return (
                  <button
                    key={t.id}
                    data-testid={`library-filter-${t.id}`}
                    className={on ? 'tag-chip tag-chip--button tag-chip--on' : 'tag-chip tag-chip--button tag-chip--off'}
                    aria-pressed={on}
                    onClick={() => toggleFilter(t.id)}
                    style={{ '--tag-color': t.color_hex } as React.CSSProperties}
                  >
                    <span className="tag-chip__dot" />
                    {t.name}
                  </button>
                );
              })}
              {filterTagIds.length > 0 && (
                <button data-testid="library-filter-clear" className="btn-link" onClick={() => setFilterTagIds([])} style={{ marginLeft: 4, fontSize: 'var(--t-meta)' }}>clear</button>
              )}
            </>
          ) : undefined
        }
      />

      {noteOpen && (
        <NewNoteModal
          tags={tags}
          onClose={() => setNoteOpen(false)}
          onCreated={async (sid) => {
            setNoteOpen(false);
            await reload();
            onSelect(sid);
          }}
          onImported={async (sid, title) => {
            setNoteOpen(false);
            await reload();
            onSelect(sid);
            // Imported audio runs the full cascade (transcribe → diarize →
            // summarize).
            onStartSummarize(sid, title || sid, 'cascade');
          }}
        />
      )}

      {error && <p className="meta--danger" style={{ padding: 16 }}>Failed to load sessions: {error}</p>}

      <div
        className="library"
        ref={libRef}
        style={listWidth === null ? undefined : ({ '--library-list-w': `${listWidth}px` } as React.CSSProperties)}
      >
        <div className="library__list">
          {visible.length === 0 && !error && (sessions.length === 0 ? (
            <EmptyState
              testid="library-empty"
              title="No meetings yet"
              inset
              action={(
                <button data-testid="library-empty-record" className="btn" onClick={onStartRecording}>
                  Record a meeting
                </button>
              )}
            >
              Recorded meetings land here, newest first, each with its transcript, summary and
              notes.
            </EmptyState>
          ) : (
            <EmptyState
              testid="library-empty-filtered"
              title="No meetings carry these tags"
              inset
              action={(
                <button data-testid="library-empty-clear" className="btn" onClick={() => setFilterTagIds([])}>
                  Clear the filter
                </button>
              )}
            >
              The filter above is narrowing the list to meetings that carry every tag you
              picked. Drop one to widen it.
            </EmptyState>
          ))}
          {visible.map((s) => (
            <div
              key={s.session_id}
              data-testid={`library-row-${s.session_id}`}
              className={`session-row ${s.session_id === effectiveSelected ? 'session-row--active' : ''}`}
              onClick={() => onSelect(s.session_id)}
            >
              <span className="session-row__title">
                <span className="session-row__name">{s.title || 'Untitled'}</span>
                {s.recording_kind && (
                  <span
                    data-testid="session-kind"
                    className="kind-chip"
                    title={recordingKindLabel(s.recording_kind).title}
                  >
                    {recordingKindLabel(s.recording_kind).chip}
                  </span>
                )}
                {s.interrupted && (
                  <span
                    className="session-row__badge"
                    title="Interrupted recording (force-quit or crash) — recovered automatically. Audio + transcript were rebuilt from the partial capture."
                  >
                    interrupted
                  </span>
                )}
              </span>
              <span className="session-row__meta">
                <span className="session-row__when">
                  {fmtDate(s.created_at_unix_seconds)} {fmtTime(s.created_at_unix_seconds)}
                </span>
                <span className="session-row__dur">
                  {s.finalized_at_unix_seconds === null ? '…' : formatDurationSeconds(s.duration_seconds)}
                </span>
                <span className="session-row__marks">
                {s.tag_ids.length > 0 && (
                  <span className="session-row__tags" title={tagNames(s.tag_ids).join(', ')}>
                    {s.tag_ids.slice(0, TAG_DOTS).map((id) => {
                      const t = tagById.get(id);
                      if (!t) return null;
                      return (
                        <span
                          key={id}
                          className="session-row__tagdot"
                          style={{ '--tag-color': t.color_hex } as React.CSSProperties}
                          title={t.name}
                        />
                      );
                    })}
                    {s.tag_ids.length > TAG_DOTS && (
                      <span className="session-row__tagmore">+{s.tag_ids.length - TAG_DOTS}</span>
                    )}
                  </span>
                )}
                <RowGhosts s={s} />
                </span>
              </span>
            </div>
          ))}
        </div>
        <div className="library__divider" role="separator" aria-orientation="vertical" onMouseDown={startListDrag} />
        {effectiveSelected ? (
          <SummaryPane
            sessionId={effectiveSelected}
            key={effectiveSelected}
            focus={effectiveSelected === selectedSessionId ? focus : undefined}
            onMetaChanged={reload}
            onStartSummarize={onStartSummarize}
            isProcessing={processingSessionIds.includes(effectiveSelected)}
            onNavigateToProviders={onNavigateToProviders}
            onDeleted={(id) => {
              // Drops the selection, then refreshes the list. The list
              // refresh is the source of truth; a backend-rejected delete
              // makes the row reappear.
              if (selectedSessionId === id) onSelect('');
              reload();
            }}
          />
        ) : (
          <div className="summary-pane"><p className="meta">{sessions.length ? 'Select a session.' : ''}</p></div>
        )}
      </div>
    </div>
  );
}


function NewNoteModal({
  tags, onClose, onCreated, onImported,
}: {
  tags: Tag[];
  onClose: () => void;
  onCreated: (sessionId: string) => void | Promise<void>;
  onImported: (sessionId: string, title: string) => void | Promise<void>;
}) {
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [tagIds, setTagIds] = useState<string[]>([]);
  // Prop tags plus any created in this modal via the combobox.
  const [knownTags, setKnownTags] = useState<Tag[]>(tags);
  const [audioPath, setAudioPath] = useState<string | null>(null);
  // Speaker count for imported audio; '' = auto-detect.
  const [speakers, setSpeakers] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  // Set when an imported file decoded but looks rough — show the warning and
  // let the user proceed best-effort.
  const [warn, setWarn] = useState<{ sid: string; title: string; note: string } | null>(null);

  const audioName = audioPath ? audioPath.split(/[\\/]/).pop() : null;

  function toggleTag(id: string) {
    setTagIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  async function pickAudio() {
    const sel = await open({
      multiple: false,
      filters: [{ name: 'Audio', extensions: ['mp3', 'm4a', 'aac', 'wav', 'flac', 'ogg', 'opus'] }],
    });
    if (typeof sel === 'string') setAudioPath(sel);
  }

  async function save() {
    if (!audioPath && !body.trim()) { setErr('Add notes or an audio file.'); return; }
    setBusy(true); setErr(null);
    try {
      if (audioPath) {
        const n = parseInt(speakers, 10);
        const r = await tauri.importAudioMeeting({
          title: title.trim() || null,
          notes_md: body,
          tag_ids: tagIds,
          audio_path: audioPath,
          expected_speakers: Number.isFinite(n) && n > 0 ? n : null,
        });
        const t = title.trim() || (audioName ?? 'Imported meeting');
        if (r.quality_ok) {
          await onImported(r.session_id, t);
        } else {
          // Created, but rough audio — warn before kicking off the cascade.
          setWarn({ sid: r.session_id, title: t, note: r.quality_note });
          setBusy(false);
        }
      } else {
        const sid = await tauri.createNoteSession({
          title: title.trim() || null,
          notes_md: body,
          tag_ids: tagIds,
        });
        await onCreated(sid);
      }
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }

  if (warn) {
    return (
      <div data-testid="import-warn-scrim" className="modal-backdrop" onClick={busy ? undefined : onClose}>
        <div data-testid="import-warn" className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 480 }}>
          <h2 className="h2" style={{ marginTop: 0 }}>Audio looks rough</h2>
          <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)' }}>{warn.note}.</p>
          <p className="meta" style={{ fontSize: 'var(--t-ui)' }}>
            Daisy can still transcribe it, but accuracy and speaker separation will be
            <strong> best-effort</strong>. Continue?
          </p>
          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 16 }}>
            <button data-testid="import-warn-cancel" className="btn" onClick={onClose}>Cancel</button>
            <button data-testid="import-warn-proceed" className="btn btn--primary" onClick={() => void onImported(warn.sid, warn.title)}>
              Transcribe anyway
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div data-testid="new-meeting-scrim" className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div data-testid="new-meeting" className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 620, width: '90%' }}>
        <h2 className="h2" style={{ marginTop: 0 }}>New meeting</h2>
        <p className="meta" style={{ fontSize: 'var(--t-small)', marginBottom: 12 }}>
          Add a meeting from notes, an uploaded audio file (iPhone Voice Memo, another app…), or
          both. Audio is transcribed, diarized, and summarized; notes and transcript fold into
          search + Q&amp;A.
        </p>
        <label className="field-label">Title (optional)</label>
        <input
          data-testid="new-meeting-title"
          type="text" autoFocus value={title} disabled={busy}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="e.g. Vendor sync — pricing"
          style={{ width: '100%', marginBottom: 12 }}
        />
        <label className="field-label">Audio file (optional)</label>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
          <button data-testid="new-meeting-choose-audio" className="btn" disabled={busy} onClick={() => void pickAudio()}>
            {audioPath ? 'Change…' : 'Choose audio…'}
          </button>
          {audioName && <span className="meta" style={{ fontSize: 'var(--t-small)', wordBreak: 'break-all' }}>{audioName}</span>}
          {audioPath && (
            <button data-testid="new-meeting-clear-audio" className="btn" disabled={busy} onClick={() => setAudioPath(null)} title="Remove">✕</button>
          )}
        </div>
        {audioPath && (
          <div style={{ marginBottom: 12 }}>
            <label className="field-label">Speakers</label>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <input
                data-testid="new-meeting-speakers"
                type="number" min={1} max={20} value={speakers} disabled={busy}
                onChange={(e) => setSpeakers(e.target.value)}
                placeholder="Auto-detect"
                style={{ width: 120 }}
              />
              <span className="meta" style={{ fontSize: 'var(--t-small)' }}>
                Set when you know how many people spoke; improves voice separation.
              </span>
            </div>
          </div>
        )}
        <label className="field-label">Notes {audioPath ? '(optional)' : ''}</label>
        <textarea
          data-testid="new-meeting-notes"
          value={body} disabled={busy}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Paste or type your meeting notes…"
          style={{ width: '100%', minHeight: 220, resize: 'vertical', marginBottom: 12 }}
        />
        <div style={{ marginBottom: 12 }}>
          <label className="field-label" style={{ marginBottom: 6 }}>Tags</label>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center' }}>
            {knownTags.map((t) => {
              const on = tagIds.includes(t.id);
              return (
                <button
                  key={t.id}
                  data-testid={`new-meeting-tag-${t.id}`}
                  className={on ? 'tag-chip tag-chip--button' : 'tag-chip tag-chip--button tag-chip--off'}
                  onClick={() => toggleTag(t.id)}
                  disabled={busy}
                  style={{ '--tag-color': t.color_hex } as React.CSSProperties}
                >
                  <span className="tag-chip__dot" />
                  {t.name}
                </button>
              );
            })}
            <TagCombobox
              excludeIds={knownTags.map((t) => t.id)}
              onPick={(t) => {
                setKnownTags((prev) => (prev.some((x) => x.id === t.id) ? prev : [...prev, t]));
                setTagIds((prev) => (prev.includes(t.id) ? prev : [...prev, t.id]));
              }}
            />
          </div>
        </div>
        {err && <p className="meta meta--danger" style={{ marginBottom: 8 }}>{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button data-testid="new-meeting-cancel" className="btn" disabled={busy} onClick={onClose}>Cancel</button>
          <button data-testid="new-meeting-submit" className="btn btn--primary" disabled={busy || (!body.trim() && !audioPath)} onClick={() => void save()}>
            {busy ? (audioPath ? 'Importing…' : 'Creating…') : (audioPath ? 'Import & transcribe' : 'Create')}
          </button>
        </div>
      </div>
    </div>
  );
}
