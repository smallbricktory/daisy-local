import { useCallback, useEffect, useState } from 'react';
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

interface Props {
  onOpenSession: (id: string) => void;
  onNavigateToProviders: () => void;
}

const LAST_PROMPT_KEY = 'daisy:analyzer:lastPrompt';
export const ADHOC_ID = 'adhoc';

/** Coaching/analysis topics first, then the summary presets, then Ad Hoc. */
function orderPrompts(list: Prompt[]): Prompt[] {
  const isSummaryPreset = (p: Prompt) =>
    p.id === 'builtin:daisy' || p.id === 'builtin:zoom' || p.id === 'builtin:otter';
  return [...list.filter((p) => !isSummaryPreset(p)), ...list.filter(isSummaryPreset)];
}

export function Analyzer({ onOpenSession, onNavigateToProviders }: Props) {
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

  async function runAnalysis() {
    if (!ai.configured) { setAiModal(true); return; }
    if (!sessionId) { setErr('Pick a meeting first.'); return; }
    if (isAdhoc && !directive.trim()) { setErr('Describe what this analysis should produce.'); return; }
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
      setBusy(false);
    }
  }

  const selectedSession = sessions.find((s) => s.session_id === sessionId);

  return (
    <div style={{ padding: 'var(--space-4)', maxWidth: 880 }}>
      <h1 className="h1 h1--sticky">Analyzer</h1>
      <p className="meta" style={{ fontSize: 13, marginTop: 4, marginBottom: 20 }}>
        Run any prompt over a meeting — coaching topics, summary styles, or something you type
        ad hoc. Edit a prompt before running; save it if you want to keep it.
      </p>

      <h2 className="h2">Meeting</h2>
      {sessions.length === 0 ? (
        <p className="meta" style={{ marginTop: 8 }}>
          No meetings with a transcript yet. Record one, then come back.
        </p>
      ) : (
        <>
          <select
            value={sessionId}
            onChange={(e) => setSessionId(e.target.value)}
            disabled={busy}
            style={{ width: '100%', maxWidth: 540 }}
          >
            {sessions.map((s) => (
              <option key={s.session_id} value={s.session_id}>
                {s.title || s.session_id} — {fmtWhen(s.created_at_unix_seconds)}
              </option>
            ))}
          </select>
          {selectedSession && (
            <p className="meta" style={{ fontSize: 12, marginTop: 6 }}>
              <button onClick={() => onOpenSession(selectedSession.session_id)} className="btn-link">
                Open in Library →
              </button>
            </p>
          )}
        </>
      )}

      <h2 className="h2">Topic</h2>
      <select
        value={promptId}
        onChange={(e) => void switchPrompt(e.target.value)}
        disabled={busy}
        style={{ width: '100%', maxWidth: 540 }}
      >
        {prompts.map((p) => (
          <option key={p.id} value={p.id}>{p.name}</option>
        ))}
        <option value={ADHOC_ID}>Ad Hoc…</option>
      </select>

      <h2 className="h2">Describe what this analysis should produce</h2>
      <label style={{ display: 'block', maxWidth: 640 }}>
        <textarea
          aria-label="Describe what this analysis should produce"
          style={{ display: 'block', width: '100%' }}
          value={directive}
          rows={8}
          disabled={busy}
          onChange={(e) => setDirective(e.target.value)}
          placeholder={isAdhoc ? 'e.g. Did I hit my goals for this meeting: close the pilot scope, agree a ship date?' : undefined}
        />
      </label>
      <div style={{ display: 'flex', gap: 8, marginTop: 12, flexWrap: 'wrap', alignItems: 'center' }}>
        <button className="btn btn--primary" onClick={() => void runAnalysis()} disabled={busy || !sessionId}>
          {busy ? 'Analyzing…' : 'Run Analysis'}
        </button>
        <button className="btn" onClick={() => { setSavingAs(true); setNewName(''); }} disabled={busy || !directive.trim()}>
          Save as…
        </button>
        {sel && !sel.builtin && !isAdhoc && (
          <button className="btn" onClick={() => void save()} disabled={busy || !dirty}>Save changes</button>
        )}
      </div>
      {savingAs && (
        <div style={{ marginTop: 10 }}>
          <input
            autoFocus
            placeholder="New prompt name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') void saveAsNew(); }}
          />
          <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
            <button className="btn btn--primary" onClick={() => void saveAsNew()} disabled={!newName.trim()}>Save</button>
            <button className="btn" onClick={() => setSavingAs(false)}>Cancel</button>
          </div>
        </div>
      )}

      {err && <p style={{ color: 'var(--danger)', marginTop: 12 }}>{err}</p>}

      {result && (
        <div style={{ marginTop: 20 }}>
          <div style={{ display: 'flex', alignItems: 'baseline', gap: 8 }}>
            <h2 className="h2" style={{ margin: 0 }}>Result</h2>
            <span className="meta" style={{ fontSize: 12 }}>
              {result.prompt_name} · {fmtWhen(result.generated_at_unix_seconds)}
            </span>
            <CopyButton kind="summary" text={result.markdown} label="Copy" style={{ marginLeft: 'auto' }} />
          </div>
          <div style={{ border: '1px solid var(--frost-deep)', borderRadius: 6, padding: 14, marginTop: 8 }}>
            <MarkdownView markdown={result.markdown} />
          </div>
        </div>
      )}

      <AiProviderRequiredModal
        open={aiModal}
        feature="Analysis"
        onClose={() => setAiModal(false)}
        onOpenProviders={onNavigateToProviders}
      />
    </div>
  );
}

const WHEN_FMT = new Intl.DateTimeFormat(undefined, {
  month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit',
});
function fmtWhen(unixSeconds: number): string {
  return WHEN_FMT.format(new Date(unixSeconds * 1000));
}
