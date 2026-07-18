// Shown when the trial has ended (or a license expired). Blocks the app until
// a valid license is entered. The user's data is untouched — this gates
// features only. Buy link opens the website; license is verified offline.

import { useEffect, useState } from 'react';
import { tauri, errStr } from '../tauri';
import { PRICING_URL } from '../lib/trial-banner';

export function LicenseGate({ onActivated }: { onActivated: () => void }) {
  const [key, setKey] = useState('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [dataDir, setDataDir] = useState<string | null>(null);

  useEffect(() => {
    tauri.bootstrapStatus().then((b) => setDataDir(b.profile_dir)).catch(() => {});
  }, []);

  async function activate() {
    if (!key.trim()) return;
    setBusy(true); setErr(null);
    try {
      await tauri.activateLicense(key.trim());
      onActivated();
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }

  return (
    <div className="hero" style={{ maxWidth: 520 }}>
      <div className="hero-mark" aria-hidden="true" />
      <span className="eyebrow">Trial ended</span>
      <h1 className="h1">Your <em>Daisy</em> trial is over.</h1>
      <p className="lede">Your meetings, transcripts, and notes are safe — recording and processing are paused until you activate a license.</p>

      <section className="surface" style={{ marginTop: 24 }}>
        <button
          className="btn btn--primary"
          style={{ width: '100%', padding: 12, marginBottom: 8 }}
          onClick={() => { void tauri.openExternal(PRICING_URL); }}
        >
          Activate / Buy →
        </button>
        <button
          className="btn"
          style={{ width: '100%', marginBottom: 16 }}
          onClick={() => { void tauri.openExternal(PRICING_URL); }}
        >
          How to restore
        </button>

        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 6 }}>
          Already have a key? Paste it:
        </label>
        <textarea
          value={key}
          disabled={busy}
          onChange={(e) => setKey(e.target.value)}
          placeholder="paste your license key"
          rows={3}
          className="textarea--mono" style={{ width: '100%' }}
        />
        {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8 }}>{err}</p>}
        <button
          className="btn btn--primary"
          style={{ marginTop: 12 }}
          disabled={busy || !key.trim()}
          onClick={() => void activate()}
        >
          {busy ? 'Activating…' : 'Activate'}
        </button>
      </section>

      {dataDir && (
        <section className="surface" style={{ marginTop: 16 }}>
          <p className="meta" style={{ fontSize: 12, margin: 0 }}>
            Your recordings, transcripts, summaries, and notes are safe and untouched, in:
          </p>
          <p style={{ fontFamily: 'var(--font-mono)', fontSize: 12, wordBreak: 'break-all', margin: '6px 0 0' }}>
            {dataDir}
          </p>
        </section>
      )}
    </div>
  );
}
