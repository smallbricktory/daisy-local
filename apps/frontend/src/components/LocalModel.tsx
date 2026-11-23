// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';
import { tauri, errStr, formatBytes, type BuiltinRow } from '../tauri';
import { confirm } from '../lib/confirm';
import { RowMenu } from './RowMenu';
import { LicenseLink } from './LicenseLink';
import {
  cancelDownload,
  downloadMessage,
  enqueueDownload,
  retryDownload,
  useDownloadQueue,
} from '../downloadQueue';
import { useBuiltinModel } from '../builtinModel';

/** The built-in summary model: one row with Download / progress / Warming
 *  up / Enable / Delete, the same shape as a transcription model. Lives in
 *  Settings → Providers → AI provider, above the key editors. The row is the
 *  backend's (`builtin-model:changed`); the download queue adds the progress
 *  of a download in flight. `enabled` says the built-in model is the AI
 *  provider in use.
 *  The download belongs to the Rust queue, so it runs on while this page is
 *  unmounted and a return shows its progress. */
export function LocalModel({ enabled, onEnable }: {
  enabled: boolean;
  onEnable: () => void;
}) {
  const status = useBuiltinModel();
  const m = status?.model ?? null;
  const job = useDownloadQueue().find((j) => j.kind === 'llm');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  if (m === null) {
    return <p className="meta" style={{ fontSize: 'var(--t-ui)' }}>Loading…</p>;
  }

  const active = job && job.phase.state !== 'failed' ? job : null;
  const failed = job?.phase.state === 'failed';
  const working = busy || active !== null || m.warming;

  const deleteModel = async (opts: { title: string; body: string; confirmLabel: string }) => {
    const ok = await confirm({ ...opts, danger: true });
    if (!ok) return;
    setBusy(true); setErr(null);
    try { await tauri.builtinModelDelete(); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(false); }
  };

  const onDelete = (row: BuiltinRow) => deleteModel({
    title: `Delete ${row.display}?`,
    body: `Removes the downloaded model (${formatBytes(row.bytes)}). Summaries need another provider until it is downloaded again.`,
    confirmLabel: 'Delete',
  });

  const onRemovePartial = (row: BuiltinRow) => deleteModel({
    title: `Remove the partial download of ${row.display}?`,
    body: `Frees the ${formatBytes(row.partial_bytes)} a stopped download left on disk. Downloading again starts over.`,
    confirmLabel: 'Remove',
  });

  const run = (p: Promise<void>) => {
    setErr(null);
    p.catch((e) => setErr(errStr(e)));
  };

  const wontStart = m.installed && !m.warming && m.warm_failed !== null
    ? `The model downloaded, but would not start on this computer. ${enabled ? 'Delete it to free the space.' : 'Try Enable again, or delete it.'}`
    : null;
  const message = err ?? downloadMessage(job) ?? wontStart;
  // A wait between attempts is not a failure; only a spent job reads danger.
  const danger = err !== null || failed || (message !== null && message === wontStart);
  const progress = active
    ? active.phase.state === 'queued'
      ? 'Queued'
      : active.phase.state === 'retrying'
        ? `Retrying ${active.phase.attempt} of ${active.phase.of}`
        : active.phase.state === 'downloading' && active.phase.pct > 0
          ? `${Math.round(active.phase.pct * 100)}%`
          : 'downloading…'
    : m.warming ? 'Warming up…' : null;

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
      <div className="model-group-h">On this computer — Daisy's built-in model</div>
      {message && (
        <p className={danger ? 'meta meta--danger' : 'meta'} style={{ fontSize: 'var(--t-small)' }} data-testid="llm-message">
          {message}
        </p>
      )}
      <div className={m.installed ? 'asr-model is-on' : 'asr-model'} data-testid={`llm-row-${m.id}`}>
        <span className="mono" style={{ fontSize: 'var(--t-ui)', minWidth: 152 }}>{m.display}</span>
        <span className="meta" style={{ fontSize: 'var(--t-meta)' }} data-testid={`llm-meta-${m.id}`}>
          {m.capability_line} · {formatBytes(m.bytes)} ·{' '}
          <LicenseLink url={m.license_url} license={m.license} testId={`llm-license-${m.id}`} />
        </span>
        <span style={{ flex: 1 }} />
        {m.installed && enabled && (
          <span className="meta--success asr-model__active" data-testid={`llm-enabled-${m.id}`}>● Enabled</span>
        )}
        {progress !== null ? (
          <>
            <span className="meta" data-testid={`llm-progress-${m.id}`} style={{ fontSize: 'var(--t-small)', fontVariantNumeric: 'tabular-nums' }}>
              {progress}
            </span>
            <button type="button" data-testid={`llm-cancel-${m.id}`} className="btn" onClick={() => run(cancelDownload(m.id))}>Cancel</button>
          </>
        ) : !m.installed ? (
          <>
            {m.partial_bytes > 0 && (
              <RowMenu
                testId={`llm-partial-menu-${m.id}`}
                label={`Actions for the stopped download of ${m.display}`}
                disabled={working}
                items={[{
                  label: `Remove partial download (${formatBytes(m.partial_bytes)})`,
                  testId: `llm-remove-partial-${m.id}`,
                  danger: true,
                  onSelect: () => void onRemovePartial(m),
                }]}
              />
            )}
            {m.capability === 'unsupported' ? null : (
              <button
                type="button"
                data-testid={`llm-download-${m.id}`}
                className="btn"
                disabled={working}
                onClick={() => run(failed ? retryDownload(m.id) : enqueueDownload('llm', m.id))}
              >
                {failed ? 'Retry download' : 'Download'}
              </button>
            )}
            {failed && (
              <button type="button" data-testid={`llm-cancel-${m.id}`} className="btn" onClick={() => run(cancelDownload(m.id))}>Cancel</button>
            )}
          </>
        ) : (
          <>
            {!enabled && m.capability !== 'unsupported' && (
              <button type="button" data-testid={`llm-enable-${m.id}`} className="btn" disabled={working} onClick={onEnable}>Enable</button>
            )}
            <RowMenu
              testId={`llm-menu-${m.id}`}
              label={`Actions for ${m.display}`}
              disabled={working}
              items={[{ label: 'Delete', testId: `llm-delete-${m.id}`, danger: true, onSelect: () => void onDelete(m) }]}
            />
          </>
        )}
      </div>
    </div>
  );
}
