import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { tauri, errStr, formatBytes, type AsrModelInfo, type AsrDownloadProgress, type AsrEngine } from '../tauri';
import { confirm } from '../lib/confirm';

/** Engine group headers, in display order. Rows are data (engine-typed
 *  registry rows); nothing here branches on a specific engine. An engine
 *  missing from the order still renders, appended after the known ones. */
const ENGINE_LABELS: Record<AsrEngine, string> = {
  speechanalyzer: 'Apple Speech — built into macOS',
  parakeet: 'Parakeet — fast on-device English',
  whisper: 'Whisper — multilingual, larger models available',
};
// The speechanalyzer row is only ever listed on Apple platforms where the
// OS can transcribe (backend gates on sa_available()), so its group heads
// the list there and never appears elsewhere.
const ENGINE_ORDER: AsrEngine[] = ['speechanalyzer', 'parakeet', 'whisper'];

/** Model manager over the engine-typed registry: one list drives engine +
 *  model selection. Bundled floor rows show "Built in"; Delete removes the
 *  profile copy only (floor rows revert to the bundle). Lives in Settings →
 *  Providers → Local transcription. */
export function AsrModels() {
  const [models, setModels] = useState<AsrModelInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  // At most one download is in flight (every button disables on busy);
  // non-null while it runs.
  const [pct, setPct] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [failedId, setFailedId] = useState<string | null>(null);
  const dlReq = useRef<{ id: string; rid: string } | null>(null);

  const reload = () =>
    tauri.listAsrModels().then(setModels).catch((e) => setErr(errStr(e)))
      // The record-only banner (App.tsx) re-resolves on this.
      .finally(() => window.dispatchEvent(new Event('daisy:asr-models-changed')));
  useEffect(() => { void reload(); }, []);

  useEffect(() => {
    let un: (() => void) | undefined;
    let cancelled = false;
    listen<AsrDownloadProgress>('asr-download:progress', (e) => {
      const { request_id, downloaded, total } = e.payload;
      if (dlReq.current?.rid === request_id && total) setPct(downloaded / total);
    }).then((fn) => { if (cancelled) fn(); else un = fn; }).catch(() => {});
    return () => { cancelled = true; un?.(); };
  }, []);

  const onUse = async (id: string) => {
    setBusy(id); setErr(null);
    try { await tauri.setActiveAsrModel(id); await reload(); }
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
    try { await tauri.deleteAsrModel(m.id); await reload(); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  const onDownload = async (m: AsrModelInfo) => {
    const rid = `ad-${m.id}-${Math.random().toString(36).slice(2, 9)}`;
    dlReq.current = { id: m.id, rid };
    setBusy(m.id); setErr(null); setFailedId(null); setPct(0);
    try {
      await tauri.downloadAsrModel(rid, m.id);
      await reload();
    } catch (e) {
      setFailedId(m.id);
      setErr(`Download of ${m.display} didn't finish (${errStr(e)}). Check your internet connection and free disk space, then retry.`);
    }
    finally {
      setBusy(null);
      dlReq.current = null;
      setPct(null);
    }
  };

  const onCancel = () => {
    const rid = dlReq.current?.rid;
    if (rid) tauri.cancelAsrDownload(rid).catch(() => {});
  };

  if (models === null) {
    return <p className="meta" style={{ fontSize: 13 }}>Loading models…</p>;
  }

  const byEngine = new Map<AsrEngine, AsrModelInfo[]>();
  for (const m of models) byEngine.set(m.engine, [...(byEngine.get(m.engine) ?? []), m]);
  const order = [
    ...ENGINE_ORDER.filter((e) => byEngine.has(e)),
    ...[...byEngine.keys()].filter((e) => !ENGINE_ORDER.includes(e)),
  ];
  const groups = order.map((engine) => ({ engine, rows: byEngine.get(engine)! }));

  return (
    <div>
      <p className="meta" style={{ fontSize: 13, margin: '4px 0 10px' }}>
        One model is active at a time; switching applies to the next recording.
      </p>
      {err && <p className="meta" style={{ color: 'var(--danger)', fontSize: 12 }}>{err}</p>}
      {groups.map(({ engine, rows }) => (
        <div key={engine} style={{ marginBottom: 12 }}>
          <div className="model-group-h">{ENGINE_LABELS[engine] ?? engine}</div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
            {rows.map((m) => {
              const downloading = busy === m.id && pct !== null;
              return (
                <div
                  key={m.id}
                  style={{
                    display: 'flex', alignItems: 'center', gap: 8, padding: '6px 8px',
                    border: '1px solid var(--line)', borderRadius: 6,
                    background: m.active ? 'var(--gold-tint)' : undefined,
                  }}
                >
                  <span style={{ fontFamily: 'var(--font-mono)', fontSize: 13, minWidth: 152 }}>{m.display}</span>
                  <span style={{ fontSize: 11, color: 'var(--text-dim)' }}>
                    {m.multilingual ? 'multilingual' : 'English'}
                    {m.bytes > 0 ? ` · ${formatBytes(m.bytes)}` : ''} · {m.license}
                  </span>
                  <span style={{ flex: 1 }} />
                  {m.active && <span style={{ fontSize: 12, color: 'var(--success)', fontWeight: 600 }}>● Active</span>}
                  {m.bundled_floor && <span style={{ fontSize: 12, color: 'var(--text-dim)' }}>Built in</span>}
                  {downloading ? (
                    <>
                      {/* OS-managed installs emit no byte progress: stay
                          indeterminate and uncancellable. File transfers are
                          cancellable from the first moment — a stalled
                          connection must not trap the user. */}
                      <span style={{ fontSize: 12, color: 'var(--text-dim)', fontVariantNumeric: 'tabular-nums' }}>
                        {pct != null && pct > 0 ? `${Math.round(pct * 100)}%` : 'downloading…'}
                      </span>
                      {m.engine !== 'speechanalyzer' && (
                        <button type="button" className="btn" onClick={onCancel}>Cancel</button>
                      )}
                    </>
                  ) : !m.installed ? (
                    <button type="button" className="btn" disabled={busy !== null} onClick={() => void onDownload(m)}>
                      {failedId === m.id ? 'Retry download' : 'Download'}
                    </button>
                  ) : (
                    <>
                      {!m.active && <button type="button" className="btn" disabled={busy !== null} onClick={() => void onUse(m.id)}>Use</button>}
                      {m.profile_copy && (
                        <button type="button" className="btn btn--danger" disabled={busy !== null} onClick={() => void onDelete(m)}>Delete</button>
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
