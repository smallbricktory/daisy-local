import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { tauri, errStr, type WhisperModelInfo, type WhisperDownloadProgress } from '../tauri';
import { confirm } from '../lib/confirm';

// Approximate download sizes (upstream ggml files), shown before download.
// Exact size shows once installed.
const APPROX_MB: Record<string, number> = {
  tiny: 75, 'tiny.en': 75, base: 142, 'base.en': 142,
  small: 466, 'small.en': 466, medium: 1500, 'medium.en': 1500,
  'large-v3': 2900, 'large-v3-turbo': 1550,
};

function fmtMB(mb: number): string {
  return mb >= 1000 ? `${(mb / 1000).toFixed(1)} GB` : `${mb} MB`;
}

/** Local Whisper model manager: download larger/multilingual models, switch the
 *  active one, delete downloaded ones. The bundled base.en is the undeletable
 *  floor (always ≥1 model). Lives in Settings → Providers → Advanced. */
export function WhisperModels() {
  const [models, setModels] = useState<WhisperModelInfo[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<Record<string, number>>({});
  const [err, setErr] = useState<string | null>(null);
  const [failedSize, setFailedSize] = useState<string | null>(null);
  const reqIds = useRef<Record<string, string>>({});

  const reload = () =>
    tauri.listWhisperModels().then(setModels).catch((e) => setErr(errStr(e)));
  useEffect(() => { void reload(); }, []);

  useEffect(() => {
    let un: (() => void) | undefined;
    let cancelled = false;
    listen<WhisperDownloadProgress>('whisper-download:progress', (e) => {
      const { request_id, downloaded, total } = e.payload;
      const size = Object.entries(reqIds.current).find(([, id]) => id === request_id)?.[0];
      if (size && total) setProgress((p) => ({ ...p, [size]: downloaded / total }));
    }).then((fn) => { if (cancelled) fn(); else un = fn; }).catch(() => {});
    return () => { cancelled = true; un?.(); };
  }, []);

  const onUse = async (size: string) => {
    setBusy(size); setErr(null);
    try { await tauri.setActiveWhisperModel(size); await reload(); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  const onDelete = async (size: string) => {
    const ok = await confirm({
      title: `Delete ${size}?`,
      body: 'Removes the downloaded model file. You can re-download it later.',
      confirmLabel: 'Delete', danger: true,
    });
    if (!ok) return;
    setBusy(size); setErr(null);
    try { await tauri.deleteWhisperModel(size); await reload(); }
    catch (e) { setErr(errStr(e)); } finally { setBusy(null); }
  };

  const onDownload = async (size: string) => {
    const rid = `wd-${size}-${Math.random().toString(36).slice(2, 9)}`;
    reqIds.current[size] = rid;
    setBusy(size); setErr(null); setFailedSize(null); setProgress((p) => ({ ...p, [size]: 0 }));
    try {
      await tauri.downloadWhisperModel(rid, size);
      await reload();
    } catch (e) {
      setFailedSize(size);
      setErr(`Download of ${size} didn't finish (${errStr(e)}). Check your internet connection and free disk space, then retry.`);
    }
    finally {
      setBusy(null);
      delete reqIds.current[size];
      setProgress((p) => { const n = { ...p }; delete n[size]; return n; });
    }
  };

  const onCancel = (size: string) => {
    const rid = reqIds.current[size];
    if (rid) tauri.cancelWhisperDownload(rid).catch(() => {});
  };

  if (models === null) {
    return <p className="meta" style={{ fontSize: 13 }}>Loading models…</p>;
  }

  return (
    <div>
      <p className="meta" style={{ fontSize: 13, margin: '4px 0 10px' }}>
        Daisy ships with <strong>base.en</strong> (English). Download a larger or multilingual model
        for better accuracy or non-English meetings — it's used on the next recording. Bigger = slower.
      </p>
      {err && <p className="meta" style={{ color: 'var(--danger)', fontSize: 12 }}>{err}</p>}
      <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
        {models.map((m) => {
          const downloading = busy === m.size && m.size in progress;
          const pct = Math.round((progress[m.size] ?? 0) * 100);
          const sizeLabel = m.size_bytes
            ? fmtMB(Math.round(m.size_bytes / 1e6))
            : `~${fmtMB(APPROX_MB[m.size] ?? 0)}`;
          return (
            <div
              key={m.size}
              style={{
                display: 'flex', alignItems: 'center', gap: 8, padding: '6px 8px',
                border: '1px solid var(--frost-deep)', borderRadius: 6,
                background: m.active ? 'var(--tint)' : undefined,
              }}
            >
              <span style={{ fontFamily: 'var(--font-mono)', fontSize: 13, minWidth: 132 }}>{m.size}</span>
              <span style={{ fontSize: 11, color: 'var(--muted)' }}>
                {m.multilingual ? 'multilingual' : 'English'} · {sizeLabel}
              </span>
              <span style={{ flex: 1 }} />
              {m.active && <span style={{ fontSize: 12, color: '#2ea043', fontWeight: 600 }}>● Active</span>}
              {downloading ? (
                <>
                  <span style={{ fontSize: 12, color: 'var(--muted)', fontVariantNumeric: 'tabular-nums' }}>{pct}%</span>
                  <button className="btn" onClick={() => onCancel(m.size)}>Cancel</button>
                </>
              ) : !m.installed ? (
                <button className="btn" disabled={busy !== null} onClick={() => void onDownload(m.size)}>
                  {failedSize === m.size ? 'Retry download' : 'Download'}
                </button>
              ) : (
                <>
                  {!m.active && <button className="btn" disabled={busy !== null} onClick={() => void onUse(m.size)}>Use</button>}
                  {!m.bundled && !m.active && (
                    <button className="btn btn--danger" disabled={busy !== null} onClick={() => void onDelete(m.size)}>Delete</button>
                  )}
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
