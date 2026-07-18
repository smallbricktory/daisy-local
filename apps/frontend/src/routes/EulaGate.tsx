// First-run EULA gate. Shows the Terms of Service + Privacy Policy with a
// clickwrap "I have read and agree" checkbox. Persists acceptance via
// tauri.acceptEula() and calls onAccepted. Rendered by App.tsx's 'eula' phase,
// before the recording-consent gate (Consent.tsx).

import { useState } from 'react';
import { tauri } from '../tauri';
import { MarkdownView } from '../components/MarkdownView';
import {
  LEGAL_LAST_UPDATED,
  TOS_MARKDOWN,
  PRIVACY_MARKDOWN,
} from '../legalContent';

interface Props {
  onAccepted: () => void;
}

type Tab = 'tos' | 'privacy';

const TABS: { key: Tab; label: string }[] = [
  { key: 'tos', label: 'Terms of Service' },
  { key: 'privacy', label: 'Privacy Policy' },
];

export function EulaGate({ onAccepted }: Props) {
  const [tab, setTab] = useState<Tab>('tos');
  const [agreed, setAgreed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function accept() {
    if (!agreed) return;
    setBusy(true); setErr(null);
    try {
      await tauri.acceptEula();
      onAccepted();
    } catch (e) {
      setErr(String((e as { message?: unknown })?.message ?? e));
      setBusy(false);
    }
  }

  return (
    <div
      style={{
        maxWidth: 720,
        margin: '0 auto',
        padding: '48px 24px',
        height: '100vh',
        boxSizing: 'border-box',
        display: 'flex',
        flexDirection: 'column',
        color: 'var(--ink)',
        background: '#fff',
      }}
    >
      <h1 className="h1" style={{ marginBottom: 8, color: 'var(--ink)' }}>Terms &amp; Privacy</h1>
      <p style={{ fontSize: 14, lineHeight: 1.6, marginBottom: 20, color: 'var(--ink)' }}>
        Please review and accept to use Daisy. Last updated {LEGAL_LAST_UPDATED}.
      </p>

      <div style={{ display: 'flex', gap: 8, marginBottom: 12 }}>
        {TABS.map((t) => {
          const active = tab === t.key;
          return (
            <button
              key={t.key}
              type="button"
              className="doc-tab"
              disabled={busy}
              onClick={() => setTab(t.key)}
              style={{
                padding: '8px 16px',
                borderRadius: 8,
                border: '1px solid ' + (active ? '#000' : '#ccc'),
                background: active ? '#f0f0f0' : 'transparent',
                color: 'var(--ink)',
                fontWeight: active ? 600 : 400,
                cursor: 'pointer',
              }}
            >
              {t.label}
            </button>
          );
        })}
      </div>

      <div
        style={{
          flex: 1,
          minHeight: 0,
          maxHeight: '50vh',
          overflowY: 'auto',
          border: '1px solid #ccc',
          borderRadius: 8,
          padding: '16px 20px',
          color: 'var(--ink)',
        }}
      >
        <MarkdownView markdown={tab === 'tos' ? TOS_MARKDOWN : PRIVACY_MARKDOWN} mono />
      </div>

      <label
        style={{
          display: 'flex', gap: 12, alignItems: 'flex-start',
          padding: '14px 16px', marginTop: 16, borderRadius: 8,
          border: '1px solid ' + (agreed ? '#000' : '#ccc'),
          background: agreed ? '#f0f0f0' : 'transparent',
          color: 'var(--ink)',
          cursor: 'pointer',
        }}
      >
        <input
          type="checkbox"
          checked={agreed}
          disabled={busy}
          onChange={(e) => setAgreed(e.target.checked)}
          style={{ marginTop: 3 }}
        />
        <span style={{ fontSize: 14 }}>
          I have read and agree to the Terms of Service and Privacy Policy.
        </span>
      </label>

      {err && <p style={{ color: 'var(--ink)', fontWeight: 600, marginTop: 12 }}>{err}</p>}

      <button
        type="button"
        className="btn btn--primary"
        disabled={!agreed || busy}
        onClick={() => void accept()}
        style={{ marginTop: 16, width: '100%', justifyContent: 'center' }}
      >
        {busy ? 'Saving…' : agreed ? 'I agree — continue' : 'Check the box to continue'}
      </button>
    </div>
  );
}
