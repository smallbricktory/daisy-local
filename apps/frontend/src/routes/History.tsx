// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import React, { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { fmtDateTimeYear } from '../lib/format';
import {
  tauri,
  type IntegrationHistoryEntry,
  type QueuedWorkflowRun,
  type WorkflowRunEvent,
  type WorkflowRunRecord,
} from '../tauri';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';

interface Props {
  onOpenSession: (id: string) => void;
  onOpenWorkflows: () => void;
}

const PAGE = 200;

/** One merged timeline row: a manual integration send or a workflow run. */
type HistoryRow =
  | { kind: 'manual'; at: number; entry: IntegrationHistoryEntry }
  | { kind: 'run'; at: number; run: WorkflowRunRecord };

/** Green when the send or run succeeded, red when it did not. */
function statusClass(ok: boolean): string {
  return ok ? 'meta--success' : 'meta--danger';
}

export function History({ onOpenSession, onOpenWorkflows }: Props) {
  const [manual, setManual] = useState<IntegrationHistoryEntry[]>([]);
  const [runs, setRuns] = useState<WorkflowRunRecord[] | null>(null);
  const [queued, setQueued] = useState<QueuedWorkflowRun[]>([]);
  const [liveStep, setLiveStep] = useState<WorkflowRunEvent | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [moreExhausted, setMoreExhausted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const manualCount = useRef(0);
  const runCount = useRef(0);

  const reload = () => {
    setError(null);
    Promise.all([tauri.integrationHistory(PAGE, 0), tauri.workflowHistory(PAGE, 0), tauri.workflowQueueState()])
      .then(([m, r, q]) => {
        setManual(m);
        setRuns(r);
        setQueued(q);
        manualCount.current = m.length;
        runCount.current = r.length;
        setMoreExhausted(m.length < PAGE && r.length < PAGE);
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

  // Pages BOTH stores; each keeps its own offset so the merged timeline
  // never skips the older store's entries.
  const loadMore = () => {
    Promise.all([
      tauri.integrationHistory(PAGE, manualCount.current),
      tauri.workflowHistory(PAGE, runCount.current),
    ])
      .then(([moreManual, moreRuns]) => {
        setManual((cur) => [...cur, ...moreManual]);
        setRuns((cur) => [...(cur ?? []), ...moreRuns]);
        manualCount.current += moreManual.length;
        runCount.current += moreRuns.length;
        if (moreManual.length < PAGE && moreRuns.length < PAGE) setMoreExhausted(true);
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
    <div className="page">
      <PageHeader
        title="History"
        subtitle="Workflow runs and meetings pushed to your outbound destinations."
        actions={<button data-testid="history-refresh" onClick={reload} className="btn">Refresh</button>}
      />
      <div className="page__body page__measure">
      {error && <p className="meta meta--danger">{error}</p>}

      {queued.length > 0 && (
        <div className="run-queue">
          {queued.map((q) => {
            const live = liveStep && liveStep.run_id === q.run_id ? liveStep : null;
            return (
              <div key={q.run_id} className="run-queue__row">
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
        <EmptyState
          testid="history-empty"
          title="Nothing has run yet"
          action={(
            <button data-testid="history-empty-workflows" className="btn" onClick={onOpenWorkflows}>
              Create a workflow
            </button>
          )}
        >
          Workflow runs and manual sends both land in this list, with the step-by-step result
          of each. Use “Send to…” on a meeting to make an entry by hand.
        </EmptyState>
      )}

      {rows.length > 0 && (
        <table className="data-table data-table--top">
          <thead>
            <tr>
              <th>When</th>
              <th>Meeting</th>
              <th>Source</th>
              <th>Detail</th>
              <th>Result</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => {
              if (row.kind === 'manual') {
                const r = row.entry;
                const ok = r.status === 'ok';
                return (
                  <tr key={`m-${r.at_unix_seconds}-${r.integration_id}-${r.session_id}`}>
                    <td className="data-table__when">{fmtDateTimeYear(r.at_unix_seconds)}</td>
                    <td>
                      <button data-testid={`history-session-open-${r.session_id}`} className="btn-link" onClick={() => onOpenSession(r.session_id)}>
                        {r.meeting_title || r.session_id}
                      </button>
                    </td>
                    <td>Manual</td>
                    <td>
                      {r.integration_name} <span className="meta">({r.kind})</span>
                      {r.payloads_sent.length > 0 && <span className="meta"> · {r.payloads_sent.join(', ')}</span>}
                    </td>
                    <td className={statusClass(ok)}>{ok ? 'OK' : r.status}</td>
                  </tr>
                );
              }
              const r = row.run;
              const ok = r.status === 'ok';
              const open = expanded.has(r.run_id);
              return (
                <React.Fragment key={`r-${r.run_id}`}>
                  <tr className={open ? 'data-table__row--open' : undefined}>
                    <td className="data-table__when">{fmtDateTimeYear(r.at_unix_seconds)}</td>
                    <td>
                      <button data-testid={`history-run-open-${r.session_id}`} className="btn-link" onClick={() => onOpenSession(r.session_id)}>
                        {r.session_title || r.session_id}
                      </button>
                    </td>
                    <td>{r.workflow_name}</td>
                    <td>
                      <button
                        data-testid={`history-expand-${r.run_id}`}
                        className="btn-link"
                        aria-label={`Expand run ${r.run_id}`}
                        onClick={() => toggleExpand(r.run_id)}
                      >
                        {open ? '▾' : '▸'} {r.steps.length} step{r.steps.length === 1 ? '' : 's'}
                      </button>
                    </td>
                    <td className={statusClass(ok)}>{ok ? 'OK' : r.status}</td>
                  </tr>
                  {open && (
                    <tr>
                      <td />
                      <td colSpan={4} className="run-steps">
                        {r.steps.length === 0 && <span className="meta">No steps ran.</span>}
                        {r.steps.map((s, i) => (
                          <div key={i} className="run-step">
                            <span className="run-step__n">{i + 1}.</span>
                            <span className="run-step__label">{s.label}</span>
                            <span className="run-step__ms">{s.duration_ms} ms</span>
                            <span className={statusClass(s.status === 'ok')}>{s.status}</span>
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

      {!moreExhausted && runs && (manual.length >= PAGE || runs.length >= PAGE) && (
        <button data-testid="history-more" className="btn load-more" onClick={loadMore}>Load more</button>
      )}
      </div>
    </div>
  );
}
