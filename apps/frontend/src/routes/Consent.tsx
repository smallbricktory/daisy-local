// First-run consent gate. Machine-local (shows once per install). Every item
// must be checked before the user can enter the app. Acknowledges legal
// responsibility for recording, explains diarization, and confirms the user
// has the right permissions.

import { useState } from 'react';
import { tauri } from '../tauri';

interface Props {
  onAccepted: () => void;
}

const ITEMS: { key: string; title: string; body: string }[] = [
  {
    key: 'laws',
    title: 'Recording laws are my responsibility',
    body:
      'Recording-consent laws vary by country, state, and province — some require all parties to consent. ' +
      'I understand it is solely my responsibility to know and follow the laws that apply to me and the people I record.',
  },
  {
    key: 'consent',
    title: 'I will obtain the consent I need',
    body:
      'Before recording a meeting, I will get whatever permission the law and my participants require, ' +
      'and I accept full responsibility for any consequences of recording without proper consent.',
  },
  {
    key: 'diarization',
    title: 'I understand voice identification (diarization & voiceprints)',
    body:
      'Daisy can separate speakers and, if I enroll them, store voice embeddings ("voiceprints") to recognize ' +
      'people across meetings. Voiceprints are biometric data. I will only identify or enroll people I have ' +
      'permission to, and I understand diarization is automatic and can make mistakes.',
  },
  {
    key: 'data',
    title: 'I am responsible for the recordings and how they are used',
    body:
      'Recordings, transcripts, and summaries are stored where I configure (which may include cloud-synced folders ' +
      'that are NOT end-to-end encrypted). I am responsible for securing this data and for anything I do with it, ' +
      'including any AI summaries I share.',
  },
];

export function Consent({ onAccepted }: Props) {
  const [checked, setChecked] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const allChecked = ITEMS.every((i) => checked[i.key]);

  async function accept() {
    if (!allChecked) return;
    setBusy(true); setErr(null);
    try {
      await tauri.acceptConsent();
      onAccepted();
    } catch (e) {
      setErr(String((e as { message?: unknown })?.message ?? e));
      setBusy(false);
    }
  }

  return (
    <div style={{ maxWidth: 640, margin: '0 auto', padding: '48px 24px' }}>
      <h1 className="h1" style={{ marginBottom: 8 }}>Before you start</h1>
      <p className="meta" style={{ fontSize: 14, lineHeight: 1.6, marginBottom: 24 }}>
        Daisy records and transcribes meetings. Please read and acknowledge each item.
        All are required to use the app.
      </p>

      <div style={{ display: 'flex', flexDirection: 'column', gap: 14 }}>
        {ITEMS.map((item) => (
          <label
            key={item.key}
            style={{
              display: 'flex', gap: 12, alignItems: 'flex-start',
              padding: '14px 16px', borderRadius: 8,
              border: '1px solid ' + (checked[item.key] ? 'var(--indigo-deep)' : 'var(--frost-deep)'),
              background: checked[item.key] ? 'var(--cream-pure)' : 'transparent',
              cursor: 'pointer',
            }}
          >
            <input
              type="checkbox"
              checked={!!checked[item.key]}
              disabled={busy}
              onChange={(e) => setChecked((c) => ({ ...c, [item.key]: e.target.checked }))}
              style={{ marginTop: 3 }}
            />
            <span>
              <strong style={{ fontSize: 14 }}>{item.title}</strong>
              <br />
              <span className="meta" style={{ fontSize: 13, lineHeight: 1.55 }}>{item.body}</span>
            </span>
          </label>
        ))}
      </div>

      {err && <p className="meta" style={{ color: 'var(--danger)', marginTop: 16 }}>{err}</p>}

      <button
        className="btn btn--primary"
        disabled={!allChecked || busy}
        onClick={() => void accept()}
        style={{ marginTop: 24, width: '100%', padding: '12px' }}
      >
        {busy ? 'Saving…' : allChecked ? 'I acknowledge — continue' : 'Check every item to continue'}
      </button>
    </div>
  );
}
