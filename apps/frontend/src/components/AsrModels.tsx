// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { tauri, errStr, formatBytes, type AsrModelInfo, type AsrEngine } from '../tauri';
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
import { reloadAsrModels, useAsrModels } from '../asrModels';

/** Engine group headers, in display order. Rows are data; an engine missing from the order is appended after the known ones. */
const ENGINE_LABELS: Record<AsrEngine, string> = {
  speechanalyzer: 'Apple Speech — built into macOS',
  nemotron: 'Nemotron — fast on-device English',
  whisper: 'Whisper — multilingual, larger models available',
};
// The speechanalyzer row is only ever listed on Apple platforms where the
// OS can transcribe (backend gates on sa_available()), so its group heads
// the list there and never appears elsewhere.
const ENGINE_ORDER: AsrEngine[] = ['speechanalyzer', 'nemotron', 'whisper'];

/** Model manager over the engine-typed registry: one list drives engine +
 *  model selection. Bundled floor rows show "Built in"; Delete removes the
 *  profile copy only (floor rows revert to the bundle). Lives in Settings →
 *  Providers → Local transcription. The download belongs to the Rust queue,
 *  so it runs on while this page is unmounted and a return shows its
 *  progress. Whisper is a Nerd Mode extra unless it's the active engine; the
 *  platform's default engine (Apple Speech or Nemotron) always shows. */
export function AsrModels({ nerd = false }: { nerd?: boolean }) {
  const jobs = useDownloadQueue();
  const job = jobs.find((j) => j.kind === 'asr' || j.kind === 'sa-assets');
  const models = useAsrModels();
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // The rows follow `models:changed`; a read that fails is shown here.
  useEffect(() => { reloadAsrModels().catch((e) => setErr(errStr(e))); }, []);

  const working = busy !== null || (job != null && job.phase.state !== 'failed');

  const onEnable = async (id: string) => {
    setBusy(id); setErr(null);
    try { await tauri.setActiveAsrModel(id); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  const onDelete = async (m: AsrModelInfo) => {
    const ok = await confirm({
      title: `Delete ${m.display}?`,
      body: m.bundled_floor
        ? 'Removes the downloaded copy; the built-in copy stays available.'
        : m.active
          ? 'Removes the downloaded model; transcription falls back to the default. You can re-download it later.'
          : 'Removes the downloaded model. You can re-download it later.',
      confirmLabel: 'Delete', danger: true,
    });
    if (!ok) return;
    setBusy(m.id); setErr(null);
    try { await tauri.deleteAsrModel(m.id); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  if (models === null) {
    return <p className="meta" style={{ fontSize: 'var(--t-ui)' }}>Loading models…</p>;
  }

  const byEngine = new Map<AsrEngine, AsrModelInfo[]>();
  for (const m of models) byEngine.set(m.engine, [...(byEngine.get(m.engine) ?? []), m]);
  // Whisper hides without Nerd Mode unless it's the active engine — a
  // profile switched to Whisper while Nerd Mode was on keeps showing it.
  const order = [
    ...ENGINE_ORDER.filter((e) => byEngine.has(e)
      && (nerd || e !== 'whisper' || byEngine.get(e)!.some((m) => m.active))),
    ...[...byEngine.keys()].filter((e) => !ENGINE_ORDER.includes(e)),
  ];
  const groups = order.map((engine) => ({ engine, rows: byEngine.get(engine)! }));
  const message = err ?? downloadMessage(job);
  // A wait between attempts is not a failure; only a spent job reads danger.
  const danger = err !== null || job?.phase.state === 'failed';

  return (
    <div>
      {message && (
        <p className={danger ? 'meta meta--danger' : 'meta'} style={{ fontSize: 'var(--t-small)' }}>
          {message}
        </p>
      )}
      {groups.map(({ engine, rows }) => (
        <div key={engine} style={{ marginBottom: 12 }}>
          <div className="model-group-h">{ENGINE_LABELS[engine] ?? engine}</div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            {rows.map((m) => {
              const active = job?.id === m.id && job.phase.state !== 'failed' ? job : null;
              const failed = job?.id === m.id && job.phase.state === 'failed';
              const pct = active?.phase.state === 'downloading' ? active.phase.pct : 0;
              return (
                <div
                  key={m.id}
                  className={m.active ? 'asr-model is-on' : 'asr-model'}
                >
                  <span className="mono" style={{ fontSize: 'var(--t-ui)', minWidth: 152 }}>{m.display}</span>
                  <span className="meta" style={{ fontSize: 'var(--t-meta)' }}>
                    {m.multilingual ? 'multilingual' : 'English'}
                    {m.bytes > 0 ? ` · ${formatBytes(m.bytes)}` : ''} ·{' '}
                    <LicenseLink url={m.license_url} license={m.license} testId={`asr-license-${m.id}`} />
                  </span>
                  <span style={{ flex: 1 }} />
                  {m.active && <span className="meta--success asr-model__active" data-testid={`asr-enabled-${m.id}`}>● Enabled</span>}
                  {m.bundled_floor && <span className="meta" style={{ fontSize: 'var(--t-small)' }}>Built in</span>}
                  {active ? (
                    <>
                      {/* OS-managed installs emit no byte progress: stay
                          indeterminate and uncancellable. File transfers are
                          cancellable from the first moment. */}
                      <span className="meta" style={{ fontSize: 'var(--t-small)', fontVariantNumeric: 'tabular-nums' }}>
                        {active.phase.state === 'queued'
                          ? 'Queued'
                          : active.phase.state === 'retrying'
                            ? `Retrying ${active.phase.attempt} of ${active.phase.of}`
                            : active.indeterminate || pct === 0 ? 'downloading…' : `${Math.round(pct * 100)}%`}
                      </span>
                      {m.engine !== 'speechanalyzer' && (
                        <button type="button" data-testid={`asr-cancel-${m.id}`} className="btn" onClick={() => { void cancelDownload(m.id); }}>Cancel</button>
                      )}
                    </>
                  ) : !m.installed ? (
                    <>
                      <button
                        type="button"
                        data-testid={`asr-download-${m.id}`}
                        className="btn"
                        disabled={working}
                        onClick={() => {
                          void (failed
                            ? retryDownload(m.id)
                            : enqueueDownload(m.engine === 'speechanalyzer' ? 'sa-assets' : 'asr', m.id));
                        }}
                      >
                        {failed ? 'Retry download' : 'Download'}
                      </button>
                      {failed && (
                        <button type="button" data-testid={`asr-cancel-${m.id}`} className="btn" onClick={() => { void cancelDownload(m.id); }}>Cancel</button>
                      )}
                    </>
                  ) : (
                    <>
                      {!m.active && <button type="button" data-testid={`asr-enable-${m.id}`} className="btn" disabled={working} onClick={() => void onEnable(m.id)}>Enable</button>}
                      {m.profile_copy && (
                        <RowMenu
                          testId={`asr-menu-${m.id}`}
                          label={`Actions for ${m.display}`}
                          disabled={working}
                          items={[{
                            label: 'Delete',
                            testId: `asr-delete-${m.id}`,
                            danger: true,
                            onSelect: () => void onDelete(m),
                          }]}
                        />
                      )}
                    </>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      ))}
    </div>
  );
}
