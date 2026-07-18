import React, { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  tauri,
  type IntegrationHistoryEntry,
  type QueuedWorkflowRun,
  type WorkflowRunEvent,
  type WorkflowRunRecord,
} from '../tauri';

interface Props {
  onOpenSession: (id: string) => void;
}

const PAGE = 200;

// Cached formatter — fresh Intl.DateTimeFormat per call dominates CPU on
// long histories. One instance, many .format() calls.
const WHEN_FMT = new Intl.DateTimeFormat(undefined, {
  year: 'numeric', month: 'short', day: 'numeric',
  hour: 'numeric', minute: '2-digit',
});
function fmtWhen(unix: number): string {
  return WHEN_FMT.format(new Date(unix * 1000));
}

const mono: React.CSSProperties = { fontFamily: 'var(--font-mono)', fontSize: 12 };

/** One merged timeline row: a manual integration send or a workflow run. */
type HistoryRow =
  | { kind: 'manual'; at: number; entry: IntegrationHistoryEntry }
  | { kind: 'run'; at: number; run: WorkflowRunRecord };

function statusColor(ok: boolean): string {
  return ok ? 'var(--success)' : 'var(--danger)';
}

const td: React.CSSProperties = { padding: '6px 12px' };

export function History({ onOpenSession }: Props) {
  const [manual, setManual] = useState<IntegrationHistoryEntry[]>([]);
  const [runs, setRuns] = useState<WorkflowRunRecord[] | null>(null);
  const [queued, setQueued] = useState<QueuedWorkflowRun[]>([]);
  const [liveStep, setLiveStep] = useState<WorkflowRunEvent | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [moreExhausted, setMoreExhausted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const runCount = useRef(0);

  const reload = () => {
    setError(null);
    Promise.all([tauri.integrationHistory(PAGE), tauri.workflowHistory(PAGE, 0), tauri.workflowQueueState()])
      .then(([m, r, q]) => {
        setManual(m);
        setRuns(r);
        setQueued(q);
        runCount.current = r.length;
        setMoreExhausted(r.length < PAGE);
      })
      .catch((e) => setError(String(e)));
  };
  useEffect(() => { reload(); }, []);

  // Live updates: step ticks refresh the in-flight line; terminal events move
  // a run from the queue into history — refetch everything.
  useEffect(() => {
    const un = listen<WorkflowRunEvent>('workflow:run', ({ payload }) => {
      if (payload.status === 'running') {
        setLiveStep(payload);
      } else {
        setLiveStep(null);
        reload();
      }
    });
    return () => { void un.then((f) => f()); };
  }, []);

  const loadMore = () => {
    tauri.workflowHistory(PAGE, runCount.current)
      .then((more) => {
        setRuns((cur) => [...(cur ?? []), ...more]);
        runCount.current += more.length;
        if (more.length < PAGE) setMoreExhausted(true);
      })
      .catch((e) => setError(String(e)));
  };

  const rows: HistoryRow[] = [
    ...manual.map((entry): HistoryRow => ({ kind: 'manual', at: entry.at_unix_seconds, entry })),
    ...(runs ?? []).map((run): HistoryRow => ({ kind: 'run', at: run.at_unix_seconds, run })),
  ].sort((a, b) => b.at - a.at);

  const toggleExpand = (runId: string) => {
    setExpanded((cur) => {
      const next = new Set(cur);
      if (next.has(runId)) next.delete(runId); else next.add(runId);
      return next;
    });
  };

  return (
    <div style={{ padding: 'var(--space-4)', maxWidth: 880 }}>
      <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 12, position: 'sticky', top: 0, zIndex: 5, background: 'var(--cream)', paddingTop: 4 }}>
        <h1 className="h1">History</h1>
        <button onClick={reload} className="btn">Refresh</button>
      </div>
      <p className="meta" style={{ fontSize: 13, marginTop: 4 }}>
        Workflow runs and meetings pushed to your outbound destinations.
      </p>

      {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 12 }}>{error}</p>}

      {queued.length > 0 && (
        <div style={{ marginTop: 16 }}>
          {queued.map((q) => {
            const live = liveStep && liveStep.run_id === q.run_id ? liveStep : null;
            return (
              <div key={q.run_id} style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '6px 0' }}>
                <span className="meta">⏳</span>
                <span>{q.workflow_name}</span>
                <span className="meta">
                  {q.session_title || q.session_id} —{' '}
                  {live
                    ? `step ${live.step_index + 1} of ${live.step_count}: ${live.step_label}`
                    : 'queued'}
                </span>
              </div>
            );
          })}
        </div>
      )}

      {runs && rows.length === 0 && queued.length === 0 && (
        <p className="meta" style={{ marginTop: 24 }}>
          Nothing here yet. Workflow runs and manual sends both land in this list — create a
          workflow in Workflows, or use “Send to…” on a meeting.
        </p>
      )}

      {rows.length > 0 && (
        <table style={{ width: '100%', marginTop: 16, borderCollapse: 'collapse', fontSize: 13 }}>
          <thead>
            <tr style={{ textAlign: 'left', color: 'var(--muted)', borderBottom: '1px solid var(--frost-deep)' }}>
              <th style={{ padding: '6px 12px 6px 0', whiteSpace: 'nowrap' }}>When</th>
              <th style={td}>Meeting</th>
              <th style={td}>Source</th>
              <th style={td}>Detail</th>
              <th style={{ padding: '6px 0' }}>Result</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              if (row.kind === 'manual') {
                const r = row.entry;
                const ok = r.status === 'ok';
                return (
                  <tr key={`m-${r.at_unix_seconds}-${r.integration_id}-${r.session_id}`} style={{ borderBottom: '1px solid var(--frost-soft)' }}>
                    <td style={{ padding: '6px 12px 6px 0', whiteSpace: 'nowrap', color: 'var(--muted)' }}>{fmtWhen(r.at_unix_seconds)}</td>
                    <td style={td}>
                      <button className="btn-link" onClick={() => onOpenSession(r.session_id)}>
                        {r.meeting_title || r.session_id}
                      </button>
                    </td>
                    <td style={td}>Manual</td>
                    <td style={td}>
                      {r.integration_name} <span className="meta">({r.kind})</span>
                      {r.payloads_sent.length > 0 && <span className="meta"> · {r.payloads_sent.join(', ')}</span>}
                    </td>
                    <td style={{ padding: '6px 0', color: statusColor(ok) }}>{ok ? 'OK' : r.status}</td>
                  </tr>
                );
              }
              const r = row.run;
              const ok = r.status === 'ok';
              const open = expanded.has(r.run_id);
              return (
                <React.Fragment key={`r-${r.run_id}`}>
                  <tr style={{ borderBottom: open ? 'none' : '1px solid var(--frost-soft)' }}>
                    <td style={{ padding: '6px 12px 6px 0', whiteSpace: 'nowrap', color: 'var(--muted)' }}>{fmtWhen(r.at_unix_seconds)}</td>
                    <td style={td}>
                      <button className="btn-link" onClick={() => onOpenSession(r.session_id)}>
                        {r.session_title || r.session_id}
                      </button>
                    </td>
                    <td style={td}>{r.workflow_name}</td>
                    <td style={td}>
                      <button
                        className="btn-link"
                        aria-label={`Expand run ${r.run_id}`}
                        onClick={() => toggleExpand(r.run_id)}
                      >
                        {open ? '▾' : '▸'} {r.steps.length} step{r.steps.length === 1 ? '' : 's'}
                      </button>
                    </td>
                    <td style={{ padding: '6px 0', color: statusColor(ok) }}>{ok ? 'OK' : r.status}</td>
                  </tr>
                  {open && (
                    <tr style={{ borderBottom: '1px solid var(--frost-soft)' }}>
                      <td />
                      <td colSpan={4} style={{ padding: '0 12px 8px' }}>
                        {r.steps.length === 0 && <span className="meta">No steps ran.</span>}
                        {r.steps.map((s, i) => (
                          <div key={i} style={{ display: 'flex', gap: 10, alignItems: 'baseline', padding: '2px 0' }}>
                            <span style={mono}>{i + 1}.</span>
                            <span style={{ flex: 1 }}>{s.label}</span>
                            <span style={mono}>{s.duration_ms} ms</span>
                            <span style={{ color: statusColor(s.status === 'ok') }}>{s.status}</span>
                          </div>
                        ))}
                      </td>
                    </tr>
                  )}
                </React.Fragment>
              );
            })}
          </tbody>
        </table>
      )}

      {!moreExhausted && runs && runs.length >= PAGE && (
        <button className="btn" style={{ marginTop: 12 }} onClick={loadMore}>Load more</button>
      )}
    </div>
  );
}
