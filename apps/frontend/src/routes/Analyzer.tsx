// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useRef, useState } from 'react';
import { fmtDateTime } from '../lib/format';
import { confirm } from '../lib/confirm';
import { setNavGuard } from '../lib/navGuard';
import {
  tauri,
  errStr,
  type AnalysisResult,
  type Prompt,
  type SessionListEntry,
} from '../tauri';
import { useAiProviderStatus } from '../lib/aiProviderStatus';
import { AiProviderRequiredModal } from '../components/AiProviderRequiredModal';
import { MarkdownView } from '../components/MarkdownView';
import { CopyButton } from './SummaryPane';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';
import { Field } from '../components/Field';
import { Select } from '../components/Select';

interface Props {
  onOpenSession: (id: string) => void;
  onNavigateToProviders: () => void;
  /** Navigate to the recorder — the step an analyzer with nothing to read offers. */
  onStartRecording: () => void;
}

const LAST_PROMPT_KEY = 'daisy:analyzer:lastPrompt';
export const ADHOC_ID = 'adhoc';

/** Coaching/analysis topics first, then the summary presets, then Ad Hoc. */
function orderPrompts(list: Prompt[]): Prompt[] {
  const isSummaryPreset = (p: Prompt) =>
    p.id === 'builtin:daisy' || p.id === 'builtin:zoom' || p.id === 'builtin:otter';
  return [...list.filter((p) => !isSummaryPreset(p)), ...list.filter(isSummaryPreset)];
}

export function Analyzer({ onOpenSession, onNavigateToProviders, onStartRecording }: Props) {
  const ai = useAiProviderStatus();
  const [aiModal, setAiModal] = useState(false);
  const [sessions, setSessions] = useState<SessionListEntry[]>([]);
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [sessionId, setSessionId] = useState<string>('');
  const [promptId, setPromptId] = useState<string>('');
  const [directive, setDirective] = useState('');
  const [result, setResult] = useState<AnalysisResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [savingAs, setSavingAs] = useState(false);
  const [newName, setNewName] = useState('');

  const sel = prompts.find((p) => p.id === promptId) ?? null;
  const isAdhoc = promptId === ADHOC_ID;
  const dirty = isAdhoc ? directive.trim().length > 0 : sel != null && directive !== sel.directive_md;

  const reload = useCallback(async () => {
    try {
      const [s, p] = await Promise.all([tauri.listSessions(), tauri.listPrompts()]);
      const eligible = s
        .filter((x) => x.has_transcript || x.has_dedup)
        .sort((a, b) => b.created_at_unix_seconds - a.created_at_unix_seconds);
      setSessions(eligible);
      const ordered = orderPrompts(p);
      setPrompts(ordered);
      setSessionId((cur) => cur || eligible[0]?.session_id || '');
      setPromptId((cur) => {
        if (cur && (cur === ADHOC_ID || ordered.some((x) => x.id === cur))) return cur;
        const last = localStorage.getItem(LAST_PROMPT_KEY);
        if (last && (last === ADHOC_ID || ordered.some((x) => x.id === last))) return last;
        return ordered[0]?.id ?? ADHOC_ID;
      });
    } catch (e) {
      setErr(errStr(e));
    }
  }, []);
  useEffect(() => { void reload(); }, [reload]);

  // Sync the editor with the selected prompt + persist last-selected.
  useEffect(() => {
    if (promptId) localStorage.setItem(LAST_PROMPT_KEY, promptId);
    if (isAdhoc) { setDirective(''); return; }
    const p = prompts.find((x) => x.id === promptId);
    if (p) setDirective(p.directive_md);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [promptId, prompts.length]);

  // Load any saved artifact for (session, prompt).
  useEffect(() => {
    setResult(null);
    if (!sessionId || !promptId) return;
    tauri.analysisLoad(sessionId, promptId).then((r) => setResult(r ?? null)).catch(() => { /* fine */ });
  }, [sessionId, promptId]);

  // Global unsaved-edit guard: ask before leaving the page via the nav rail.
  // Ad-hoc text is intentionally NOT guarded (running one-time is the flow).
  const guarded = dirty && !isAdhoc;
  useEffect(() => {
    if (!guarded) { setNavGuard(null); return; }
    setNavGuard(() => confirm({
      title: 'Discard unsaved prompt edits?',
      body: 'You edited this prompt but did not save it.',
      confirmLabel: 'Discard',
      danger: true,
    }));
    return () => setNavGuard(null);
  }, [guarded]);

  async function switchPrompt(id: string) {
    if (dirty && !isAdhoc) {
      const ok = await confirm({
        title: 'Discard unsaved changes?',
        body: 'This prompt has unsaved edits.',
        confirmLabel: 'Discard',
        danger: true,
      });
      if (!ok) return;
    }
    setPromptId(id);
    setSavingAs(false);
    setErr(null);
  }

  async function save() {
    if (!sel || sel.builtin || isAdhoc) return;
    try {
      await tauri.savePrompt({ id: sel.id, name: sel.name, directive_md: directive.trim(), output: sel.output });
      setPrompts((cur) => cur.map((p) => (p.id === sel.id ? { ...p, directive_md: directive.trim() } : p)));
    } catch (e) {
      setErr(errStr(e));
    }
  }

  async function saveAsNew() {
    if (!newName.trim() || !directive.trim()) return;
    try {
      const output = sel?.output ?? 'sectioned';
      const created = await tauri.savePrompt({ id: null, name: newName.trim(), directive_md: directive.trim(), output });
      setSavingAs(false);
      setNewName('');
      const p = await tauri.listPrompts();
      setPrompts(orderPrompts(p));
      setPromptId(created.id);
    } catch (e) {
      setErr(errStr(e));
    }
  }

  // Synchronous re-entrancy guard: a double-click fires two click handlers
  // before the `busy` state re-render can disable the button.
  const runningRef = useRef(false);

  async function runAnalysis() {
    if (runningRef.current) return;
    if (!ai.configured) {
      if (ai.state === 'Installing') { setErr(ai.hint ?? "Daisy's model is still downloading."); return; }
      setAiModal(true);
      return;
    }
    if (!sessionId) { setErr('Pick a meeting first.'); return; }
    if (isAdhoc && !directive.trim()) { setErr('Describe what this analysis should produce.'); return; }
    runningRef.current = true;
    setErr(null);
    setBusy(true);
    try {
      // A clean stored prompt runs by id; Ad Hoc or edited text runs the text
      // as-is (no forced save).
      const out = dirty || isAdhoc
        ? await tauri.runAnalysis({ session_id: sessionId, directive_md: directive })
        : await tauri.runAnalysis({ session_id: sessionId, prompt_id: promptId });
      setResult(out);
    } catch (e) {
      setErr(errStr(e));
    } finally {
      runningRef.current = false;
      setBusy(false);
    }
  }

  const selectedSession = sessions.find((s) => s.session_id === sessionId);

  return (
    <div className="page">
      <PageHeader
        title="Analyzer"
        subtitle="Run any prompt over a meeting — pick a saved one, edit it, or type one here."
      />
      <div className="page__body page__measure">
      {sessions.length === 0 ? (
        <EmptyState
          testid="analyzer-empty"
          title="No meetings to analyze yet"
          inset
          action={(
            <button data-testid="analyzer-empty-record" className="btn" onClick={onStartRecording}>
              Record a meeting
            </button>
          )}
        >
          The Analyzer runs a prompt over one meeting&apos;s transcript, so it opens once a
          recording has been transcribed.
        </EmptyState>
      ) : (
        <>
          <Field label="Meeting" htmlFor="analyzer-session">
          <Select
            id="analyzer-session"
            testId="analyzer-session"
            className="field__control"
            value={sessionId}
            onChange={setSessionId}
            disabled={busy}
            options={sessions.map((s) => ({
              value: s.session_id,
              label: `${s.title || s.session_id} — ${fmtDateTime(s.created_at_unix_seconds)}`,
            }))}
          />
          {selectedSession && (
            <p className="meta" style={{ fontSize: 'var(--t-small)', marginTop: 6 }}>
              <button data-testid="analyzer-open-library" onClick={() => onOpenSession(selectedSession.session_id)} className="btn-link">
                Open in Library →
              </button>
            </p>
          )}
          </Field>
        </>
      )}

      <Field label="Topic" htmlFor="analyzer-prompt">
      <Select
        id="analyzer-prompt"
        testId="analyzer-prompt"
        className="field__control"
        value={promptId}
        onChange={(v) => void switchPrompt(v)}
        disabled={busy}
        options={[
          ...prompts.map((p) => ({ value: p.id, label: p.name })),
          { value: ADHOC_ID, label: 'Ad Hoc…' },
        ]}
      />
      </Field>

      <Field label="Describe what this analysis should produce" htmlFor="analyzer-directive">
        <textarea
          id="analyzer-directive"
          data-testid="analyzer-directive"
          className="field__control"
          value={directive}
          rows={8}
          disabled={busy}
          onChange={(e) => setDirective(e.target.value)}
          placeholder={isAdhoc ? 'e.g. Did I hit my goals for this meeting: close the pilot scope, agree a ship date?' : undefined}
        />
      </Field>
      <div style={{ display: 'flex', gap: 8, marginTop: 12, flexWrap: 'wrap', alignItems: 'center' }}>
        <button data-testid="analyzer-run" className="btn btn--primary" onClick={() => void runAnalysis()} disabled={busy || !sessionId}>
          {busy ? 'Analyzing…' : 'Run Analysis'}
        </button>
        <button data-testid="analyzer-save-as" className="btn" onClick={() => { setSavingAs(true); setNewName(''); }} disabled={busy || !directive.trim()}>
          Save as…
        </button>
        {sel && !sel.builtin && !isAdhoc && (
          <button data-testid="analyzer-save" className="btn" onClick={() => void save()} disabled={busy || !dirty}>Save changes</button>
        )}
      </div>
      {savingAs && (
        <div style={{ marginTop: 10 }}>
          <input
            data-testid="analyzer-new-name"
            autoFocus
            placeholder="New prompt name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') void saveAsNew(); }}
          />
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button data-testid="analyzer-new-save" className="btn btn--primary" onClick={() => void saveAsNew()} disabled={!newName.trim()}>Save</button>
            <button data-testid="analyzer-new-cancel" className="btn" onClick={() => setSavingAs(false)}>Cancel</button>
          </div>
        </div>
      )}

      {err && <p className="meta--danger" style={{ marginTop: 12 }}>{err}</p>}

      {result && (
        <div style={{ marginTop: 20 }}>
          <div style={{ display: 'flex', alignItems: 'baseline', gap: 8 }}>
            <h2 className="h2" style={{ margin: 0 }}>Result</h2>
            <span className="meta" style={{ fontSize: 'var(--t-small)' }}>
              {result.prompt_name} · {fmtDateTime(result.generated_at_unix_seconds)}
            </span>
            <span className="row-trail" />
            <CopyButton kind="summary" text={result.markdown} />
          </div>
          <div data-testid="analyzer-output" className="inset-box" style={{ marginTop: 8 }}>
            <MarkdownView markdown={result.markdown} />
          </div>
        </div>
      )}

      </div>

      <AiProviderRequiredModal
        open={aiModal}
        feature="Analysis"
        onClose={() => setAiModal(false)}
        onOpenProviders={onNavigateToProviders}
      />
    </div>
  );
}
