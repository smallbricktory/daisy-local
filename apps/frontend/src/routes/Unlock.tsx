import { useState } from 'react';
import { tauri } from '../tauri';

interface Props {
  onUnlocked: () => void;
}

export function Unlock({ onUnlocked }: Props) {
  const [pass, setPass] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [show, setShow] = useState(false);
  const [showReset, setShowReset] = useState(false);

  async function submit() {
    if (busy || pass.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      await tauri.unlockVault(pass);
      onUnlocked();
    } catch (e: unknown) {
      setError('Wrong passphrase. Try again.');
      setPass('');
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="hero" style={{ maxWidth: 520 }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
        <div className="hero-mark" aria-hidden="true" style={{ flexShrink: 0, margin: 0 }} />
        <h1 className="h1" style={{ margin: 0 }}>Daisy is <em>locked</em>.</h1>
      </div>
      <span className="eyebrow" style={{ marginTop: 12, display: 'block' }}>
        Remember · Recognize · Recall
      </span>
      <p className="lede" style={{ marginTop: 8 }}>Enter your vault passphrase.</p>

      <section className="surface" style={{ marginTop: 20 }}>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 6 }}>
          Passphrase
        </label>
        <input
          type={show ? 'text' : 'password'}
          value={pass}
          onChange={(e) => setPass(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
          disabled={busy}
          autoFocus
          style={{ display: 'block', width: '100%' }}
        />

        <label style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 12, fontSize: 13, color: 'var(--muted)' }}>
          <input
            type="checkbox"
            checked={show}
            onChange={(e) => setShow(e.target.checked)}
            disabled={busy}
          />
          Show passphrase
        </label>

        <div style={{ marginTop: 20 }}>
          <button
            className="btn btn--primary"
            onClick={submit}
            disabled={busy || pass.length === 0}
            style={{ opacity: busy || pass.length === 0 ? 0.55 : 1 }}
          >
            {busy ? 'Unlocking…' : 'Unlock'}
          </button>
        </div>

        <div style={{ marginTop: 14 }}>
          <button
            type="button"
            onClick={() => setShowReset(true)}
            disabled={busy}
            className="btn-link" style={{ fontSize: 13, color: 'var(--muted)' }}
          >
            Forgot passphrase?
          </button>
        </div>

        {error && (
          <p className="meta" style={{ color: 'var(--danger)', marginTop: 16 }}>
            {error}
          </p>
        )}
      </section>

      {showReset && (
        <ResetVaultModal
          onCancel={() => setShowReset(false)}
          onResetComplete={() => {
            setShowReset(false);
            onUnlocked();
          }}
        />
      )}
    </div>
  );
}

interface ResetModalProps {
  onCancel: () => void;
  onResetComplete: () => void;
}

function ResetVaultModal({ onCancel, onResetComplete }: ResetModalProps) {
  const [confirmText, setConfirmText] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canReset = confirmText === 'RESET' && !busy;

  async function reset() {
    if (!canReset) return;
    setBusy(true);
    setError(null);
    try {
      await tauri.resetVault();
      onResetComplete();
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" onClick={busy ? undefined : onCancel}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">There is no recovery.</div>
        <p style={{ fontSize: 14, lineHeight: 1.5, marginTop: 8 }}>
          Your encrypted API keys and webhook tokens are protected by your passphrase.
          We can&apos;t reset it — only destroy the vault and start over.
        </p>
        <p style={{ fontSize: 14, lineHeight: 1.5, marginTop: 12 }}>
          Your meetings (recordings, transcripts, summaries, notes, tags) are <strong>not</strong> encrypted
          and will stay intact. You&apos;ll only need to re-enter your provider keys after the reset.
        </p>
        <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginTop: 16, marginBottom: 6 }}>
          Type <code>RESET</code> to confirm:
        </label>
        <input
          type="text"
          value={confirmText}
          onChange={(e) => setConfirmText(e.target.value)}
          disabled={busy}
          placeholder="RESET"
          autoFocus
          style={{ display: 'block', width: 200 }}
        />
        {error && (
          <p className="meta" style={{ color: 'var(--danger)', marginTop: 12 }}>
            {error}
          </p>
        )}
        <div className="modal__actions" style={{ marginTop: 20 }}>
          <button
            className="btn btn--primary"
            onClick={reset}
            disabled={!canReset}
            style={{
              background: canReset ? 'var(--danger)' : undefined,
              borderColor: canReset ? 'var(--danger)' : undefined,
              opacity: canReset ? 1 : 0.55,
            }}
          >
            {busy ? 'Resetting…' : 'Reset vault and start over'}
          </button>
          <button className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
