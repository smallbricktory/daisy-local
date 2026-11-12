// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { fmtDateYear, fmtHms } from '../lib/format';
import { useTags } from '../lib/useTags';
import {
  tauri,
  type MatchSnippet,
  type QaAnswer,
  type SessionFocus,
  type SessionFocusTab,
  type SessionHit,
  type SessionHitSource,
  type Contact,
} from '../tauri';
import { TagChip } from '../components/tags/TagChip';
import { useAiProviderStatus } from '../lib/aiProviderStatus';
import { AiProviderRequiredModal } from '../components/AiProviderRequiredModal';
import { useQa, askQuestion as fireAsk, cancelQuestion, resetQa } from '../lib/qaStore';
import { hitSnippets } from '../lib/searchHits';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';

interface Props {
  /** Seed query (e.g. from the Library header search box). Pre-fills the box
   *  on mount and runs immediately — keyword via the debounce effect, or a
   *  Q&A ask if it ends with '?'. */
  initialQuery?: string;
  onOpenSession: (id: string, focus?: SessionFocus) => void;
  onNavigateToProviders: () => void;
}

/** Which detail tab a given match source opens; clicking a snippet lands on
 *  that view. Transcript/notes also carry the query; the transcript view
 *  scrolls to + highlights the matched line. */
function focusForSource(source: SessionHitSource, query: string): SessionFocus | undefined {
  const tab: SessionFocusTab | undefined =
    source === 'transcript' ? 'transcript'
    : source === 'notes' ? 'notes'
    : source === 'summary' || source === 'action' ? 'summary'
    : source === 'attendee' || source === 'tag' ? 'participants'
    : undefined; // title / metadata → default landing
  if (!tab) return undefined;
  return { tab, query: query.trim() || undefined };
}

const SOURCE_LABEL: Record<SessionHitSource, string> = {
  title: 'title',
  transcript: 'transcript',
  summary: 'summary',
  notes: 'notes',
  action: 'action item',
  attendee: 'attendee',
  tag: 'tag',
  metadata: '—',
};

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function tokensFromQuery(q: string): string[] {
  // Mirror the backend tokenizer enough to highlight matches client-side:
  // bare words + double-quoted phrases.
  const tokens: string[] = [];
  let current = '';
  let inQuote = false;
  for (const ch of q) {
    if (ch === '"') {
      if (current.trim()) tokens.push(current.trim());
      current = '';
      inQuote = !inQuote;
      continue;
    }
    if (!inQuote && /\s/.test(ch)) {
      if (current.trim()) tokens.push(current.trim());
      current = '';
      continue;
    }
    current += ch;
  }
  if (current.trim()) tokens.push(current.trim());
  return tokens.filter((t) => t.length > 0);
}


export function Search({ initialQuery, onOpenSession, onNavigateToProviders }: Props) {
  const ai = useAiProviderStatus();
  const [aiModal, setAiModal] = useState(false);
  const allTags = useTags();
  const tags = useMemo(
    () => [...allTags].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' })),
    [allTags],
  );
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [people, setPeople] = useState<Contact[]>([]);
  const [selectedPeople, setSelectedPeople] = useState<Set<string>>(new Set());
  const [fromIso, setFromIso] = useState('');
  const [toIso, setToIso] = useState('');
  const [text, setText] = useState(initialQuery ?? '');
  const [results, setResults] = useState<SessionHit[]>([]);
  const [searched, setSearched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showFilters, setShowFilters] = useState(false);

  // Q&A mode: any query that ends with `?` flips Search into RAG mode.
  // The first call indexes any new transcripts (visible delay). The ask
  // itself lives in the qaStore singleton and survives navigating away and
  // back (see qaStore.ts).
  const qaState = useQa();
  const qaBusy = qaState.status === 'asking';
  const qa: QaAnswer | null = qaState.status === 'done' ? qaState.answer : null;
  const qaError = qaState.status === 'error' ? qaState.error : null;
  const isQaMode = text.trim().endsWith('?');

  // Leaving Q&A mode clears a finished/failed answer. An in-flight ask is
  // left alone; a text change does not cancel it.
  useEffect(() => {
    if (!isQaMode && (qaState.status === 'done' || qaState.status === 'error'
        || qaState.status === 'cancelled')) {
      resetQa();
    }
  }, [text, isQaMode, qaState.status]);

  // Load contacts for the people filter.
  useEffect(() => {
    tauri.listContacts()
      .then((cs) => setPeople([...cs].sort((a, b) => a.display_name.localeCompare(b.display_name, undefined, { sensitivity: 'base' }))))
      .catch(() => { /* fine */ });
  }, []);

  const runSearch = useCallback(async (
    sel: Set<string>,
    ppl: Set<string>,
    txt: string,
    from: string,
    to: string,
  ) => {
    setError(null);
    setBusy(true);
    try {
      const hits = await tauri.searchSessions({
        query: txt.trim() || undefined,
        tag_ids: sel.size ? [...sel] : undefined,
        contact_ids: ppl.size ? [...ppl] : undefined,
        date_from: from ? Math.floor(Date.parse(from) / 1000) : undefined,
        date_to: to ? Math.floor(Date.parse(to) / 1000) + 86399 : undefined,
      });
      setResults(hits);
      setSearched(true);
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
      setResults([]);
      setSearched(true);
    } finally {
      setBusy(false);
    }
  }, []);

  // Debounces typed queries. While in Q&A mode (text ends with '?'), the
  // keyword pass is suppressed and stale keyword results are wiped. The
  // moment the user removes the '?', the keyword pass picks up again.
  useEffect(() => {
    if (isQaMode) {
      setResults([]);
      setSearched(false);
      return;
    }
    const id = window.setTimeout(() => {
      void runSearch(selected, selectedPeople, text, fromIso, toIso);
    }, 200);
    return () => window.clearTimeout(id);
  }, [runSearch, selected, selectedPeople, text, fromIso, toIso, isQaMode]);

  function askQuestion() {
    const q = text.trim();
    if (!q) return;
    if (!ai.configured) {
      if (ai.state === 'Installing') { setError(ai.hint ?? "Daisy's model is still downloading."); return; }
      setAiModal(true);
      return;
    }
    setError(null);
    // Fire-and-forget into the store; it owns busy/answer/error state and keeps
    // running even if the user navigates away from Search.
    void fireAsk(q);
  }

  // Seeds the query from the Library header search box. Keyword queries
  // auto-run via the debounce effect once `text` is set; Q&A queries (ending
  // '?') are suppressed there and the ask fires explicitly. Once, on mount.
  const seededRef = useRef(false);
  useEffect(() => {
    if (seededRef.current) return;
    seededRef.current = true;
    if (initialQuery && initialQuery.trim().endsWith('?')) askQuestion();
  }, [initialQuery]); // eslint-disable-line react-hooks/exhaustive-deps

  function toggleTag(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  }

  function togglePerson(id: string) {
    setSelectedPeople((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  }

  const tokens = useMemo(() => tokensFromQuery(text), [text]);
  const highlightRegex = useMemo(() => {
    if (tokens.length === 0) return null;
    const pat = tokens.map(escapeRegExp).join('|');
    return new RegExp(`(${pat})`, 'ig');
  }, [tokens]);

  function highlight(s: string): React.ReactNode {
    if (!highlightRegex) return s;
    const parts = s.split(highlightRegex);
    return parts.map((part, i) => (
      i % 2 === 1 ? <mark key={i}>{part}</mark> : <span key={i}>{part}</span>
    ));
  }

  return (
    <div className="page">
      <PageHeader title="Search" />
      <div className="page__body page__measure">
      <div style={{ display: 'flex', gap: 8 }}>
        <input
          data-testid="search-query"
          autoFocus
          type="text"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter' && isQaMode) void askQuestion(); }}
          placeholder="Search your meetings, or end with ? to ask a question"
          style={{ width: '100%' }}
        />
        {isQaMode && (
          <button
            data-testid="search-ask"
            className="btn btn--primary"
            onClick={() => askQuestion()}
            disabled={qaBusy || text.trim().length < 4}
            aria-busy={qaBusy}
            style={{ whiteSpace: 'nowrap' }}
          >
            {qaBusy ? 'Thinking…' : 'Ask'}
          </button>
        )}
        {isQaMode && qaBusy && (
          <button
            data-testid="search-cancel"
            className="btn"
            onClick={() => cancelQuestion()}
            style={{ whiteSpace: 'nowrap' }}
          >
            Cancel
          </button>
        )}
      </div>
      <p className="meta search-syntax">
        Every word has to match &middot; <code>"quote"</code> an exact phrase
      </p>

      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', marginTop: 12 }}>
        <button
          data-testid="search-filters"
          className="btn"
          onClick={() => setShowFilters((v) => !v)}
        >
          {showFilters ? 'Hide filters ▴' : 'Filters ▾'}
          {(selected.size + (fromIso ? 1 : 0) + (toIso ? 1 : 0)) > 0 && (
            <span className="meta" style={{ marginLeft: 8, fontSize: 'var(--t-meta)' }}>
              ({selected.size + (fromIso ? 1 : 0) + (toIso ? 1 : 0)} active)
            </span>
          )}
        </button>
        <span className="meta" style={{ fontSize: 'var(--t-small)' }}>
          {busy ? 'Searching…' : (searched ? `${results.length} match${results.length === 1 ? '' : 'es'}` : '')}
        </span>
      </div>

      {showFilters && (
        <div className="panel-card" style={{ marginTop: 10 }}>
          <div>
            <label className="meta" style={{ display: 'block', fontSize: 'var(--t-small)', marginBottom: 6 }}>Tags</label>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
              {tags.length === 0 && <span className="meta" style={{ fontSize: 'var(--t-small)' }}>(no tags yet)</span>}
              {tags.map((t) => selected.has(t.id) ? (
                <TagChip key={t.id} tag={t} onRemove={() => toggleTag(t.id)} />
              ) : (
                <span
                  key={t.id}
                  data-testid={`search-tag-${t.id}`}
                  className="tag-chip tag-chip--ghost"
                  style={{ cursor: 'pointer' }}
                  onClick={() => toggleTag(t.id)}
                >○ {t.name}</span>
              ))}
            </div>
          </div>
          <div style={{ marginTop: 12 }}>
            <label className="meta" style={{ display: 'block', fontSize: 'var(--t-small)', marginBottom: 6 }}>People</label>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
              {people.length === 0 && <span className="meta" style={{ fontSize: 'var(--t-small)' }}>(no people yet)</span>}
              {people.map((p) => {
                const on = selectedPeople.has(p.id);
                return (
                  <span
                    key={p.id}
                    data-testid={`search-person-${p.id}`}
                    className={on ? 'tag-chip' : 'tag-chip tag-chip--ghost'}
                    style={on ? {
                      cursor: 'pointer',
                      background: 'var(--gold-tint)',
                      border: '1px solid var(--gold)',
                      color: 'var(--gold-text)',
                      fontWeight: 600,
                    } : { cursor: 'pointer' }}
                    title={p.emails.join(', ')}
                    onClick={() => togglePerson(p.id)}
                  >{on ? '✕' : '○'} {p.display_name}</span>
                );
              })}
            </div>
          </div>
          <div style={{ marginTop: 12, display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap' }}>
            <label className="meta" style={{ fontSize: 'var(--t-small)' }}>From</label>
            <input data-testid="search-from" type="date" value={fromIso} onChange={(e) => setFromIso(e.target.value)} />
            <label className="meta" style={{ fontSize: 'var(--t-small)' }}>To</label>
            <input data-testid="search-to" type="date" value={toIso} onChange={(e) => setToIso(e.target.value)} />
            {(fromIso || toIso) && (
              <button data-testid="search-clear-dates" className="btn" onClick={() => { setFromIso(''); setToIso(''); }}>Clear dates</button>
            )}
          </div>
        </div>
      )}

      {(error || qaError) && (
        <p className="meta meta--danger" style={{ marginTop: 12 }}>{error || qaError}</p>
      )}

      {qaBusy && qaState.partial && (
        <div className="panel-card" style={{ marginTop: 18 }}>
          <div className="eyebrow eyebrow--accent" style={{ marginBottom: 8 }}>Answer · streaming…</div>
          <div className="qa-answer">{qaState.partial}</div>
        </div>
      )}

      {qa && (
        <div data-testid="search-answer" className="panel-card" style={{ marginTop: 18 }}>
          <div className="eyebrow eyebrow--accent" style={{ marginBottom: 8 }}>Answer · {qa.indexed_sessions} indexed · {qa.total_chunks} chunks</div>
          <div className="qa-answer">{qa.answer}</div>
          {qa.citations.length > 0 && (
            <>
              <div className="eyebrow" style={{ marginTop: 14 }}>Sources</div>
              <ul style={{ paddingLeft: 18, margin: '6px 0 0', fontSize: 'var(--t-ui)' }}>
                {qa.citations.map((c, i) => (
                  <li key={i} style={{ marginBottom: 6 }}>
                    <button
                      data-testid={`search-citation-${i}`}
                      onClick={() => onOpenSession(c.session_id, {
                        tab: 'transcript',
                        seekMs: c.start_ms ?? undefined,
                      })}
                      className="btn-link"
                      title={c.start_ms != null ? 'Open the recording at this moment' : 'Open the recording'}
                    >
                      {c.session_title || c.session_id}
                      {c.created_at_unix_seconds != null && (
                        <> · {new Date(c.created_at_unix_seconds * 1000).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })}</>
                      )}
                      {c.start_ms != null && <> · {fmtHms((c.start_ms) / 1000)} ▸</>}
                    </button>
                    <span className="meta" style={{ marginLeft: 8, fontSize: 'var(--t-meta)' }}>
                      score {c.score.toFixed(2)}
                    </span>
                    <div className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 2, lineHeight: 'var(--lh-text)' }}>
                      {c.excerpt.length > 220 ? c.excerpt.slice(0, 220) + '…' : c.excerpt}
                    </div>
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}

      <div style={{ marginTop: 18 }}>
        {searched && results.length === 0 && !error && (
          <EmptyState testid="search-empty" title="No matches" inset>
            Try fewer words, or end the query with a question mark to ask across every
            meeting instead.
          </EmptyState>
        )}
        {results.map((hit) => {
          const snippets = hitSnippets(hit);
          return (
          <div
            key={hit.session_id}
            data-testid={`search-result-${hit.session_id}`}
            onClick={() => onOpenSession(hit.session_id)}
            className="search-hit"
          >
            <span className="search-hit__date">{fmtDateYear(hit.created_at_unix_seconds)}</span>
            <div>
              <div className="search-hit__title">
                {highlight(hit.title || hit.session_id)}
              </div>
              {snippets.length === 0 && hit.match_source === 'metadata' && (
                <div className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 4 }}>
                  matched on metadata (date / tag filter)
                </div>
              )}
              {snippets.map((m: MatchSnippet, i) => {
                const focus = focusForSource(m.source, text);
                return (
                  <div
                    key={i}
                    data-testid={`search-match-${hit.session_id}-${i}`}
                    onClick={focus ? (e) => { e.stopPropagation(); onOpenSession(hit.session_id, focus); } : undefined}
                    title={focus ? `Open ${SOURCE_LABEL[m.source] ?? m.source}` : undefined}
                    className="search-hit__snippet"
                    style={{ cursor: focus ? 'pointer' : undefined }}
                  >
                    <span className="eyebrow" style={{ marginRight: 8 }}>
                      {SOURCE_LABEL[m.source] ?? m.source}
                    </span>
                    {highlight(m.snippet)}
                  </div>
                );
              })}
            </div>
          </div>
          );
        })}
      </div>

      </div>

      <AiProviderRequiredModal
        open={aiModal}
        feature="Q&A"
        onClose={() => setAiModal(false)}
        onOpenProviders={onNavigateToProviders}
      />
    </div>
  );
}
