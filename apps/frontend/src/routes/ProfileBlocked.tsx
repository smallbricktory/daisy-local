// Shown when an unlicensed (trial) install opens a profile that belongs to a
// different install — the trial carry-forward block. Two ways out: activate a
// license (then this data is yours, anywhere), or start fresh in a new folder.
// The existing data is never touched.

import { useEffect, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { tauri, errStr } from '../tauri';
import { PRICING_URL } from '../lib/trial-banner';

export function ProfileBlocked({ onResolved }: { onResolved: () => void }) {
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
    try { await tauri.activateLicense(key.trim()); onResolved(); }
    catch (e) { setErr(errStr(e)); setBusy(false); }
  }

  async function pickNewFolder() {
    const sel = await open({ directory: true, multiple: false });
    if (typeof sel !== 'string') return;
    setBusy(true); setErr(null);
    // switchProfile restarts the app into the fresh folder; the backend's
    // profile root can't be rebound in-process, so bootstrapSet alone would
    // leave the wizard writing into this (foreign) profile.
    try { await tauri.switchProfile(sel); }
    catch (e) { setErr(errStr(e)); setBusy(false); }
  }

  return (
    <div className="hero" style={{ maxWidth: 560 }}>
      <div className="hero-mark" aria-hidden="true" />
      <span className="eyebrow">Trial — fresh start required</span>
      <h1 className="h1">This data belongs to a <em>different install</em>.</h1>
      <p className="lede">
        Trials start clean and can't pick up meetings from a previous install.
        Activate a license to use this data anywhere — or start fresh in a new folder.
      </p>

      <section className="surface" style={{ marginTop: 24 }}>
        <button
          className="btn btn--primary"
          style={{ width: '100%', padding: 12, marginBottom: 16 }}
          onClick={() => { void tauri.openExternal(PRICING_URL); }}
        >
          Get a license →
        </button>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 6 }}>Have a key? Paste it:</label>
        <textarea
          value={key} disabled={busy}
          onChange={(e) => setKey(e.target.value)}
          placeholder="paste your license key"
          rows={3}
          className="textarea--mono" style={{ width: '100%' }}
        />
        <div style={{ display: 'flex', gap: 8, marginTop: 12, flexWrap: 'wrap' }}>
          <button className="btn btn--primary" disabled={busy || !key.trim()} onClick={() => void activate()}>
            {busy ? 'Working…' : 'Activate'}
          </button>
          <button className="btn" disabled={busy} onClick={() => void pickNewFolder()}>
            Start fresh in a new folder…
          </button>
        </div>
        {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 8 }}>{err}</p>}
      </section>

      {dataDir && (
        <p className="meta" style={{ fontSize: 12, marginTop: 16 }}>
          The existing data is untouched, in: <code style={{ wordBreak: 'break-all' }}>{dataDir}</code>
        </p>
      )}
    </div>
  );
}
