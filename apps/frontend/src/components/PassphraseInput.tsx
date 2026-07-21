import { useState, useMemo } from 'react';
import { zxcvbn, zxcvbnOptions } from '@zxcvbn-ts/core';
import * as zxcvbnCommonPackage from '@zxcvbn-ts/language-common';
import * as zxcvbnEnPackage from '@zxcvbn-ts/language-en';

zxcvbnOptions.setOptions({
  translations: zxcvbnEnPackage.translations,
  graphs: zxcvbnCommonPackage.adjacencyGraphs,
  dictionary: {
    ...zxcvbnCommonPackage.dictionary,
    ...zxcvbnEnPackage.dictionary,
  },
});

const SCORE_LABELS = ['very weak', 'weak', 'fair', 'strong', 'very strong'] as const;
const SCORE_COLORS = ['var(--danger)', 'var(--marigold-deep)', 'var(--amber)', 'var(--indigo)', 'var(--indigo-deep)'] as const;

interface Props {
  minChars: number;
  onSubmit: (passphrase: string) => void;
  busy?: boolean;
  submitLabel?: string;
}

export function PassphraseInput({ minChars, onSubmit, busy, submitLabel }: Props) {
  const [pass1, setPass1] = useState('');
  const [pass2, setPass2] = useState('');
  const [show, setShow] = useState(false);

  const strength = useMemo(() => (pass1.length > 0 ? zxcvbn(pass1) : null), [pass1]);
  const score: number = strength?.score ?? 0;

  const lenOk = pass1.length >= minChars;
  const scoreOk = score >= 3;
  const matches = pass1 === pass2 && pass2.length > 0;
  const canSubmit = lenOk && scoreOk && matches && !busy;

  const warning = strength?.feedback?.warning ?? '';
  const suggestions = strength?.feedback?.suggestions ?? [];

  return (
    <div>
      <p className="meta">
        Daisy encrypts your provider keys with a passphrase. You'll enter it on every launch.
        Minimum {minChars} characters and a strength score of at least "strong". Choose something you can remember — there is no recovery.
      </p>

      <div style={{ marginTop: 16 }}>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)' }}>
          Passphrase
        </label>
        <input
          type={show ? 'text' : 'password'}
          value={pass1}
          onChange={(e) => setPass1(e.target.value)}
          disabled={busy}
          autoFocus
          style={{ display: 'block', width: '100%', marginTop: 4 }}
        />

        {/* Strength meter */}
        {pass1.length > 0 && (
          <div style={{ marginTop: 8 }}>
            <div style={{ display: 'flex', gap: 4, height: 6 }}>
              {[0, 1, 2, 3].map((i) => (
                <div
                  key={i}
                  style={{
                    flex: 1,
                    borderRadius: 3,
                    background: i <= score - 1 ? SCORE_COLORS[score] : 'var(--frost-deep)',
                    transition: 'background 0.2s',
                  }}
                />
              ))}
            </div>
            <div style={{ display: 'flex', justifyContent: 'space-between', marginTop: 4, fontSize: 12 }}>
              <span style={{ color: SCORE_COLORS[score] }}>{SCORE_LABELS[score]}</span>
              <span style={{ color: lenOk ? 'var(--muted)' : 'var(--danger)' }}>
                {pass1.length} / {minChars} chars
              </span>
            </div>
            {warning && (
              <div style={{ fontSize: 12, color: 'var(--danger)', marginTop: 4 }}>{warning}</div>
            )}
            {suggestions.length > 0 && (
              <div style={{ fontSize: 12, color: 'var(--muted)', marginTop: 2 }}>{suggestions[0]}</div>
            )}
          </div>
        )}

        {pass1.length === 0 && (
          <div style={{ fontSize: 12, color: 'var(--danger)', marginTop: 4 }}>
            0 / {minChars} chars
          </div>
        )}
      </div>

      <div style={{ marginTop: 16 }}>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)' }}>
          Confirm passphrase
        </label>
        <input
          type={show ? 'text' : 'password'}
          value={pass2}
          onChange={(e) => setPass2(e.target.value)}
          disabled={busy}
          style={{ display: 'block', width: '100%', marginTop: 4 }}
        />
        {pass2.length > 0 && !matches && (
          <div style={{ fontSize: 12, color: 'var(--danger)', marginTop: 4 }}>
            Passphrases don't match
          </div>
        )}
      </div>

      <label style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 16, fontSize: 13 }}>
        <input type="checkbox" checked={show} onChange={(e) => setShow(e.target.checked)} disabled={busy} />
        Show passphrase
      </label>

      <div style={{ marginTop: 24 }}>
        <button
          className="btn"
          disabled={!canSubmit}
          onClick={() => onSubmit(pass1)}
        >
          {submitLabel ?? 'Continue'}
        </button>
      </div>
    </div>
  );
}
