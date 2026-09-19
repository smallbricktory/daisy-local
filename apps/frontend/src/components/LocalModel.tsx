// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { tauri, errStr, formatBytes, type LlmModelInfo } from '../tauri';
import { confirm } from '../lib/confirm';
import { RowMenu } from './RowMenu';
import { LicenseLink } from './LicenseLink';
import {
  cancelDownload,
  downloadMessage,
  enqueueDownload,
  retryDownload,
  useDownloadQueue,
  useDownloadsFinished,
} from '../downloadQueue';

/** The built-in summary model: one row with Download / progress / Warming
 *  up / Enable / Delete, the same shape as a transcription model. Lives in
 *  Settings → Providers → AI provider, above the key editors. `onChanged`
 *  fires after a download or delete so the page re-reads providers and the AI
 *  status. `enabled` says the built-in model is the AI provider in use. The
 *  download belongs to the Rust queue, so it runs on while this page is
 *  unmounted and a return shows its progress. */
export function LocalModel({ onChanged, enabled, onEnable }: {
  onChanged: () => void;
  enabled: boolean;
  onEnable: () => void;
}) {
  const jobs = useDownloadQueue();
  const finished = useDownloadsFinished();
  const job = jobs.find((j) => j.kind === 'llm');
  const [rows, setRows] = useState<LlmModelInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const reload = () => tauri.listLlmModels().then(setRows).catch((e) => setErr(errStr(e)));
  useEffect(() => { void reload(); }, []);

  // A download that ended — here or while this page was unmounted — changes
  // what the rows say and which providers the page can offer.
  useEffect(() => {
    if (finished === 0) return;
    void reload().then(onChanged);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [finished]);

  // A failed job stays in the queue for Retry, so it never leaves the list
  // and never bumps `finished` — read the shards-installed row directly.
  const failedId = job?.phase.state === 'failed' ? job.id : null;
  useEffect(() => {
    if (failedId) void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [failedId]);

  const working = busy !== null || (job != null && job.phase.state !== 'failed');

  const deleteModel = async (m: LlmModelInfo, opts: { title: string; body: string; confirmLabel: string }) => {
    const ok = await confirm({ ...opts, danger: true });
    if (!ok) return;
    setBusy(m.id); setErr(null);
    try { await tauri.deleteLlmModel(m.id); await reload(); onChanged(); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  const onDelete = (m: LlmModelInfo) => deleteModel(m, {
    title: `Delete ${m.display}?`,
    body: `Removes the downloaded model (${formatBytes(m.bytes)}). Summaries need another provider until it is downloaded again.`,
    confirmLabel: 'Delete',
  });

  const onRemovePartial = (m: LlmModelInfo) => deleteModel(m, {
    title: `Remove the partial download of ${m.display}?`,
    body: `Frees the ${formatBytes(m.partial_bytes)} a stopped download left on disk. Downloading again starts over.`,
    confirmLabel: 'Remove',
  });

  if (rows === null) {
    return err
      ? <p className="meta meta--danger" style={{ fontSize: 'var(--t-small)' }}>{err}</p>
      : <p className="meta" style={{ fontSize: 'var(--t-ui)' }}>Loading…</p>;
  }

  const message = err ?? downloadMessage(job);
  // A wait between attempts is not a failure; only a spent job reads danger.
  const danger = err !== null || job?.phase.state === 'failed';

  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
      <div className="model-group-h">On this computer — Daisy's built-in model</div>
      {message && (
        <p className={danger ? 'meta meta--danger' : 'meta'} style={{ fontSize: 'var(--t-small)' }}>
          {message}
        </p>
      )}
      {rows.map((m) => {
        const active = job?.id === m.id && job.phase.state !== 'failed' ? job : null;
        const failed = job?.id === m.id && job.phase.state === 'failed';
        return (
          <div key={m.id} className={m.installed ? 'asr-model is-on' : 'asr-model'} data-testid={`llm-row-${m.id}`}>
            <span className="mono" style={{ fontSize: 'var(--t-ui)', minWidth: 152 }}>{m.display}</span>
            <span className="meta" style={{ fontSize: 'var(--t-meta)' }} data-testid={`llm-meta-${m.id}`}>
              {m.capability_line} · {formatBytes(m.bytes)} ·{' '}
              <LicenseLink url={m.license_url} license={m.license} testId={`llm-license-${m.id}`} />
            </span>
            <span style={{ flex: 1 }} />
            {m.installed && enabled && (
              <span className="meta--success asr-model__active" data-testid={`llm-enabled-${m.id}`}>● Enabled</span>
            )}
            {active ? (
              <>
                <span className="meta" data-testid={`llm-progress-${m.id}`} style={{ fontSize: 'var(--t-small)', fontVariantNumeric: 'tabular-nums' }}>
                  {active.phase.state === 'queued'
                    ? 'Queued'
                    : active.phase.state === 'retrying'
                      ? `Retrying ${active.phase.attempt} of ${active.phase.of}`
                      : active.phase.state === 'warming'
                        ? 'Warming up…'
                        : active.phase.state === 'downloading' && active.phase.pct > 0
                          ? `${Math.round(active.phase.pct * 100)}%`
                          : 'downloading…'}
                </span>
                <button type="button" data-testid={`llm-cancel-${m.id}`} className="btn" onClick={() => { void cancelDownload(m.id); }}>Cancel</button>
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
                    onClick={() => { void (failed ? retryDownload(m.id) : enqueueDownload('llm', m.id)); }}
                  >
                    {failed ? 'Retry download' : 'Download'}
                  </button>
                )}
                {failed && (
                  <button type="button" data-testid={`llm-cancel-${m.id}`} className="btn" onClick={() => { void cancelDownload(m.id); }}>Cancel</button>
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
        );
      })}
    </div>
  );
}
