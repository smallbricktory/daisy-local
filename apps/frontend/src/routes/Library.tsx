import { useCallback, useEffect, useRef, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { tauri, formatDurationSeconds, errStr, type SessionFocus, type SessionListEntry, type Tag } from '../tauri';
import { SummaryPane } from './SummaryPane';
import type { JobKind } from '../lib/sessionPhase';
import { subscribeToLibrary } from '../lib/libraryEvents';
import { TagCombobox } from '../components/tags/TagCombobox';

interface Props {
  selectedSessionId?: string;
  /** Optional deep-link target (tab + transcript query) from a search result. */
  focus?: SessionFocus;
  onSelect: (id: string) => void;
  /** Navigate to the Search page, optionally pre-seeding + running a query. */
  onOpenSearch: (query?: string) => void;
  /** Forwarded to SummaryPane; passed up to App, which runs the cascade in
   *  the background. */
  onStartSummarize: (sessionId: string, title: string, kind?: JobKind, promptId?: string) => void;
  /** Session IDs with a background job running. SummaryPane disables the
   *  Generate button for a session whose cascade is still running. */
  processingSessionIds: string[];
  /** Navigate to Settings → Providers. Forwarded to SummaryPane for inline hints. */
  onNavigateToProviders?: () => void;
}

// Cached Intl.DateTimeFormat instances. toLocaleDateString /
// toLocaleTimeString construct a fresh formatter on each call; the compile
// step dominates the format step.
const DATE_FMT = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const TIME_FMT = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
function formatDate(unixSeconds: number): string {
  return DATE_FMT.format(new Date(unixSeconds * 1000));
}
function formatTime(unixSeconds: number): string {
  return TIME_FMT.format(new Date(unixSeconds * 1000));
}

function sortByNewest(rows: SessionListEntry[]): SessionListEntry[] {
  return [...rows].sort((a, b) => b.created_at_unix_seconds - a.created_at_unix_seconds);
}

// Last fetched list, module-scoped: a remount (e.g. recording → library nav)
// renders the previous list immediately instead of flashing the empty state
// while the refetch is in flight.
let cachedSessions: SessionListEntry[] = [];
let cachedTags: Tag[] = [];

export function Library({ selectedSessionId, focus, onSelect, onOpenSearch, onStartSummarize, processingSessionIds, onNavigateToProviders }: Props) {
  const [sessions, setSessionsState] = useState<SessionListEntry[]>(cachedSessions);
  const [tags, setTagsState] = useState<Tag[]>(cachedTags);
  const setSessions = (rows: SessionListEntry[]) => { cachedSessions = rows; setSessionsState(rows); };
  const setTags = (ts: Tag[]) => { cachedTags = ts; setTagsState(ts); };
  const [filterTagIds, setFilterTagIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  // Draggable width of the session list (px), persisted.
  const libRef = useRef<HTMLDivElement>(null);
  const [listWidth, setListWidth] = useState<number>(() => {
    const v = Number(localStorage.getItem('daisy.library.listWidth'));
    return Number.isFinite(v) && v >= 180 && v <= 700 ? v : 300;
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
      localStorage.setItem('daisy.library.listWidth', String(listWidthRef.current));
    };
    document.addEventListener('mousemove', move);
    document.addEventListener('mouseup', up);
  }
  const listWidthRef = useRef(listWidth);
  listWidthRef.current = listWidth;

  const reload = useCallback(async () => {
    try {
      const [rows, ts] = await Promise.all([tauri.listSessions(), tauri.listTags()]);
      setSessions(sortByNewest(rows));
      setTags(ts);
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

  const visible = filterTagIds.length === 0
    ? sessions
    : sessions.filter((s) => s.tag_ids.some((id) => filterTagIds.includes(id)));

  const inList = selectedSessionId && visible.some((s) => s.session_id === selectedSessionId);
  const effectiveSelected = inList ? selectedSessionId : (visible.length ? visible[0].session_id : undefined);

  const [noteOpen, setNoteOpen] = useState(false);

  // Expandable header search: collapsed to a "⌕ Search" affordance; clicking
  // expands an inline input. Submitting hands the query to the Search page,
  // which seeds + runs it immediately. '?' queries land in Q&A mode there.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchText, setSearchText] = useState('');
  function submitSearch() {
    const q = searchText.trim();
    if (!q) return;
    onOpenSearch(q);
  }

  function toggleFilter(id: string) {
    setFilterTagIds((prev) => (prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]));
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '8px 14px', borderBottom: '1px solid var(--frost-deep)' }}>
        <h1 className="h1" style={{ margin: 0 }}>Library</h1>
        <div style={{ marginLeft: 'auto', display: 'flex', alignItems: 'center', gap: 10 }}>
          {searchOpen ? (
            <input
              autoFocus
              className="lib-search__input"
              placeholder="Search meetings… (end with ? to ask)"
              value={searchText}
              onChange={(e) => setSearchText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') submitSearch();
                else if (e.key === 'Escape') { setSearchOpen(false); setSearchText(''); }
              }}
              onBlur={() => { if (!searchText.trim()) setSearchOpen(false); }}
            />
          ) : (
            <button
              className="lib-search__toggle"
              title="Search across all meetings"
              onClick={() => setSearchOpen(true)}
            >⌕ Search</button>
          )}
          <button
            className="btn"
            onClick={() => setNoteOpen(true)}
            title="Add a meeting from notes and/or an audio file you upload (iPhone, another app…)."
          >+ Meeting</button>
        </div>
      </div>

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

      {filterTags.length > 0 && (
        <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center', padding: '6px 14px', borderBottom: '1px solid var(--frost-deep)' }}>
          <span style={{ color: 'var(--iron)', fontFamily: 'var(--font-mono)', fontSize: 10, letterSpacing: '0.08em', textTransform: 'uppercase', marginRight: 2 }}>Filter</span>
          {filterTags.map((t) => {
            const on = filterTagIds.includes(t.id);
            return (
              <button
                key={t.id}
                className="tag-filter-chip"
                onClick={() => toggleFilter(t.id)}
                style={{
                  display: 'inline-flex', alignItems: 'center', gap: 5, padding: '2px 9px', borderRadius: 999, cursor: 'pointer', fontSize: 12,
                  border: '1px solid ' + (on ? t.color_hex : 'var(--frost-deep)'),
                  background: on ? `${t.color_hex}22` : 'transparent',
                  fontWeight: on ? 600 : 400,
                }}
              >
                <span style={{ display: 'inline-block', width: 9, height: 9, borderRadius: 2, background: t.color_hex }} />
                {t.name}
              </button>
            );
          })}
          {filterTagIds.length > 0 && (
            <button className="btn-link" onClick={() => setFilterTagIds([])} style={{ marginLeft: 4, fontSize: 11 }}>clear</button>
          )}
        </div>
      )}

      {error && <p style={{ padding: 16, color: 'var(--danger)' }}>Failed to load sessions: {error}</p>}

      <div
        className="library"
        ref={libRef}
        style={{ flex: 1, minHeight: 0, gridTemplateColumns: `${listWidth}px 6px 1fr` }}
      >
        <div className="library__list">
          {visible.length === 0 && !error && (
            <p style={{ padding: 16, color: 'var(--iron)' }}>
              {sessions.length === 0 ? 'No sessions yet — hit Record to capture your first meeting. Once you have a few, use Search above (end with ? to ask a question across all your meetings).' : 'No sessions match the selected tags.'}
            </p>
          )}
          {visible.map((s) => (
            <div
              key={s.session_id}
              className={`session-row ${s.session_id === effectiveSelected ? 'session-row--active' : ''}`}
              onClick={() => onSelect(s.session_id)}
            >
              <span className="session-row__date">
                {formatDate(s.created_at_unix_seconds)}
                <span className="session-row__time">{formatTime(s.created_at_unix_seconds)}</span>
              </span>
              <span className="session-row__title">
                <span className="session-row__name">{s.title || 'Untitled'}</span>
                {s.session_id.startsWith('daisy-import-') && (
                  <span
                    title="Imported from an audio file"
                    style={{
                      marginLeft: 6, fontSize: 10, fontWeight: 600, padding: '1px 6px',
                      borderRadius: 999, background: 'var(--frost-deep, #e6e6e6)',
                      color: 'var(--muted, #888)', letterSpacing: '0.03em',
                    }}
                  >
                    IMPORTED
                  </span>
                )}
                {s.tag_ids.length > 0 && (
                  <span className="session-row__tags">
                    {s.tag_ids.map((id) => {
                      const t = tags.find((x) => x.id === id);
                      if (!t) return null;
                      return (
                        <span
                          key={id}
                          className="session-row__tagdot"
                          style={{ background: t.color_hex }}
                          title={t.name}
                        />
                      );
                    })}
                  </span>
                )}
              </span>
              {s.interrupted && (
                <span
                  className="session-row__badge"
                  title="Interrupted recording (force-quit or crash) — recovered automatically. Audio + transcript were rebuilt from the partial capture."
                  style={{
                    fontSize: 9,
                    fontWeight: 600,
                    textTransform: 'uppercase',
                    letterSpacing: 0.3,
                    color: '#b7791f',
                    background: 'rgba(214,158,46,0.14)',
                    borderRadius: 3,
                    padding: '1px 4px',
                    marginLeft: 4,
                  }}
                >
                  interrupted
                </span>
              )}
              <span className="session-row__dur">
                {s.finalized_at_unix_seconds === null ? '…' : formatDurationSeconds(s.duration_seconds)}
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
          <div className="summary-pane"><p style={{ color: 'var(--iron)' }}>{sessions.length ? 'Select a session.' : ''}</p></div>
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
      <div className="modal-backdrop" onClick={busy ? undefined : onClose}>
        <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 480 }}>
          <h2 className="h2" style={{ marginTop: 0 }}>Audio looks rough</h2>
          <p style={{ fontSize: 14, lineHeight: 1.5 }}>{warn.note}.</p>
          <p className="meta" style={{ fontSize: 13 }}>
            Daisy can still transcribe it, but accuracy and speaker separation will be
            <strong> best-effort</strong>. Continue?
          </p>
          <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', marginTop: 16 }}>
            <button className="btn" onClick={onClose}>Cancel</button>
            <button className="btn btn--primary" onClick={() => void onImported(warn.sid, warn.title)}>
              Transcribe anyway
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 620, width: '90%' }}>
        <h2 className="h2" style={{ marginTop: 0 }}>New meeting</h2>
        <p className="meta" style={{ fontSize: 12, marginBottom: 12 }}>
          Add a meeting from notes, an uploaded audio file (iPhone Voice Memo, another app…), or
          both. Audio is transcribed, diarized, and summarized; notes and transcript fold into
          search + Q&amp;A.
        </p>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Title (optional)</label>
        <input
          type="text" autoFocus value={title} disabled={busy}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="e.g. Vendor sync — pricing"
          style={{ width: '100%', marginBottom: 12 }}
        />
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Audio file (optional)</label>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginBottom: 12 }}>
          <button className="btn" disabled={busy} onClick={() => void pickAudio()}>
            {audioPath ? 'Change…' : 'Choose audio…'}
          </button>
          {audioName && <span className="meta" style={{ fontSize: 12, wordBreak: 'break-all' }}>{audioName}</span>}
          {audioPath && (
            <button className="btn" disabled={busy} onClick={() => setAudioPath(null)} title="Remove">✕</button>
          )}
        </div>
        {audioPath && (
          <div style={{ marginBottom: 12 }}>
            <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Speakers</label>
            <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
              <input
                type="number" min={1} max={20} value={speakers} disabled={busy}
                onChange={(e) => setSpeakers(e.target.value)}
                placeholder="Auto-detect"
                style={{ width: 120 }}
              />
              <span className="meta" style={{ fontSize: 12 }}>
                Set when you know how many people spoke; improves voice separation.
              </span>
            </div>
          </div>
        )}
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>Notes {audioPath ? '(optional)' : ''}</label>
        <textarea
          value={body} disabled={busy}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Paste or type your meeting notes…"
          style={{ width: '100%', minHeight: 220, resize: 'vertical', marginBottom: 12 }}
        />
        <div style={{ marginBottom: 12 }}>
          <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 6 }}>Tags</label>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap', alignItems: 'center' }}>
            {knownTags.map((t) => {
              const on = tagIds.includes(t.id);
              return (
                <button
                  key={t.id}
                  className="tag-filter-chip"
                  onClick={() => toggleTag(t.id)}
                  disabled={busy}
                  style={{
                    display: 'inline-flex', alignItems: 'center', gap: 5, padding: '2px 9px',
                    borderRadius: 999, cursor: 'pointer', fontSize: 12,
                    border: '1px solid ' + (on ? t.color_hex : 'var(--frost-deep)'),
                    background: on ? `${t.color_hex}22` : 'transparent',
                    fontWeight: on ? 600 : 400,
                  }}
                >
                  <span style={{ display: 'inline-block', width: 9, height: 9, borderRadius: 2, background: t.color_hex }} />
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
        {err && <p className="meta" style={{ color: 'var(--danger)', marginBottom: 8 }}>{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button className="btn" disabled={busy} onClick={onClose}>Cancel</button>
          <button className="btn btn--primary" disabled={busy || (!body.trim() && !audioPath)} onClick={() => void save()}>
            {busy ? (audioPath ? 'Importing…' : 'Creating…') : (audioPath ? 'Import & transcribe' : 'Create')}
          </button>
        </div>
      </div>
    </div>
  );
}
