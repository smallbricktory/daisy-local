// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
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
    <div className="gate-page">
      <h1 className="h1" style={{ marginBottom: 8 }}>Terms &amp; Privacy</h1>
      <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginBottom: 20 }}>
        Please review and accept to use Daisy. Last updated {LEGAL_LAST_UPDATED}.
      </p>

      <div style={{ display: 'flex', gap: 8, marginBottom: 12 }}>
        {TABS.map((t) => {
          const active = tab === t.key;
          return (
            <button
              key={t.key}
              type="button"
              data-testid={`eula-tab-${t.key}`}
              className={active ? 'doc-tab is-on' : 'doc-tab'}
              disabled={busy}
              onClick={() => setTab(t.key)}
            >
              {t.label}
            </button>
          );
        })}
      </div>

      <div className="doc-pane" style={{ maxHeight: '50vh' }}>
        <MarkdownView markdown={tab === 'tos' ? TOS_MARKDOWN : PRIVACY_MARKDOWN} mono />
      </div>

      <label className={agreed ? 'consent-box is-on' : 'consent-box'}>
        <input
          type="checkbox"
          data-testid="eula-agree"
          checked={agreed}
          disabled={busy}
          onChange={(e) => setAgreed(e.target.checked)}
          style={{ marginTop: 3 }}
        />
        <span style={{ fontSize: 'var(--t-ui)' }}>
          I have read and agree to the Terms of Service and Privacy Policy.
        </span>
      </label>

      {err && <p className="gate-error">{err}</p>}

      <button
        type="button"
        data-testid="eula-accept"
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
