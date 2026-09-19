// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { tauri, errStr, type VaultTrust } from '../tauri';

interface Props {
  onUnlocked: () => void;
}

export function Unlock({ onUnlocked }: Props) {
  const [pass, setPass] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [show, setShow] = useState(false);
  const [showReset, setShowReset] = useState(false);
  const [sha, setSha] = useState<string | null>(null);
  const [trust, setTrust] = useState<VaultTrust | null>(null);
  const [switchErr, setSwitchErr] = useState<string | null>(null);
  useEffect(() => { tauri.buildInfo().then((b) => setSha(b.sha)).catch(() => {}); }, []);
  useEffect(() => { tauri.vaultTrust().then(setTrust).catch(() => {}); }, []);

  // A machine-mode vault still locked at this screen did not open with this
  // computer's key. With another computer recorded as its owner it belongs
  // there; otherwise this computer's key no longer matches it.
  const owner = trustedComputer(trust);
  const elsewhere = owner !== null;
  const keyLost = trust?.kind === 'machine' && !elsewhere;

  async function openOtherProfile() {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== 'string') return;
    setBusy(true);
    setSwitchErr(null);
    // Restarts the app on success; the catch only sees failures.
    try { await tauri.switchProfile(picked); }
    catch (e) { setSwitchErr(errStr(e)); setBusy(false); }
  }

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
    // Full-height flex wrapper; margin:auto centers the card and keeps tiny
    // windows scrollable to the top.
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflowY: 'auto' }}>
    <div className="hero" style={{ maxWidth: 520, width: '100%', margin: 'auto', padding: '24px var(--space-32)' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
        <div className="hero-mark" aria-hidden="true" style={{ flexShrink: 0, margin: 0 }} />
        <h1 className="h1" style={{ margin: 0 }}>Daisy is <em>locked</em>.</h1>
      </div>
      <span className="eyebrow" style={{ marginTop: 12, display: 'block' }}>
        Remember · Recognize · Recall
      </span>
      {elsewhere ? (
        <div data-testid="unlock-elsewhere">
          <p className="lede" style={{ marginTop: 8 }}>
            This profile opens without a passphrase only on {owner}. To open it on this computer:
          </p>
          <ol className="meta" style={{ marginTop: 8, paddingLeft: 20, lineHeight: 'var(--lh-text)' }}>
            <li>On {owner}, open Daisy and turn on Nerd Mode in Settings → Display.</li>
            <li>In Settings → Profile → Vault, choose Add a passphrase and save it.</li>
            <li>Enter that passphrase below.</li>
          </ol>
        </div>
      ) : keyLost ? (
        <div data-testid="unlock-key-lost">
          <p className="lede" style={{ marginTop: 8 }}>
            {trust?.trusted_on
              ? "This computer's key no longer opens this profile."
              : "This computer's key does not open this profile."}
          </p>
          <p className="meta" style={{ marginTop: 8, lineHeight: 'var(--lh-text)' }}>
            If you added a passphrase to this profile, enter it below. Otherwise reset the vault
            to start it over, or open a different profile.
          </p>
        </div>
      ) : (
        <p className="lede" style={{ marginTop: 8 }}>Enter your vault passphrase.</p>
      )}

      <section className="surface" style={{ marginTop: 20 }}>
        <label className="field-label" style={{ marginBottom: 6 }}>
          {elsewhere ? `Passphrase added on ${owner}` : 'Passphrase'}
        </label>
        <input
          type={show ? 'text' : 'password'}
          data-testid="unlock-passphrase"
          value={pass}
          onChange={(e) => setPass(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
          disabled={busy}
          autoFocus
          style={{ display: 'block', width: '100%' }}
        />

        <label className="meta" style={{ display: 'flex', gap: 6, alignItems: 'center', marginTop: 12 }}>
          <input
            type="checkbox"
            data-testid="unlock-show"
            checked={show}
            onChange={(e) => setShow(e.target.checked)}
            disabled={busy}
          />
          Show passphrase
        </label>

        <div style={{ marginTop: 20 }}>
          <button
            data-testid="unlock-submit"
            className="btn btn--primary"
            onClick={submit}
            disabled={busy || pass.length === 0}
          >
            {busy ? 'Unlocking…' : 'Unlock'}
          </button>
        </div>

        <div style={{ marginTop: 14, display: 'flex', gap: 16, flexWrap: 'wrap' }}>
          <button
            type="button"
            data-testid="unlock-forgot"
            onClick={() => setShowReset(true)}
            disabled={busy}
            className="btn-link meta"
          >
            {elsewhere ? `No access to ${owner}?` : keyLost ? 'No passphrase? Reset the vault' : 'Forgot passphrase?'}
          </button>
          <button
            type="button"
            data-testid="unlock-other-profile"
            onClick={() => void openOtherProfile()}
            disabled={busy}
            className="btn-link meta"
          >
            Open a different profile…
          </button>
        </div>

        {error && (
          <p data-testid="unlock-error" className="meta meta--danger" style={{ marginTop: 16 }}>
            {error}
          </p>
        )}
        {switchErr && (
          <p data-testid="unlock-switch-error" className="meta meta--danger" style={{ marginTop: 16 }}>
            {switchErr}
          </p>
        )}
      </section>

      {showReset && (
        <ResetVaultModal
          sharedWith={elsewhere ? owner : null}
          onCancel={() => setShowReset(false)}
          onResetComplete={() => {
            setShowReset(false);
            onUnlocked();
          }}
        />
      )}
    </div>
    {sha && (
      <span
        className="build-stamp"
      >
        build {sha}
      </span>
    )}
    </div>
  );
}

/** The other computer whose key opens a machine-mode vault. Null when the
 *  vault is passphrase-protected, names no computer, or names this one. */
export function trustedComputer(trust: VaultTrust | null): string | null {
  if (trust?.kind !== 'machine') return null;
  const name = trust.trusted_on?.trim();
  if (!name || name === trust.this_machine.trim()) return null;
  return name;
}

interface ResetModalProps {
  /** Set when the vault belongs to another computer: that computer's name. */
  sharedWith: string | null;
  onCancel: () => void;
  onResetComplete: () => void;
}

function ResetVaultModal({ sharedWith, onCancel, onResetComplete }: ResetModalProps) {
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
    <div data-testid="unlock-reset-backdrop" className="modal-backdrop" onClick={busy ? undefined : onCancel}>
      <div data-testid="unlock-reset-modal" className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">There is no recovery.</div>
        <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 8 }}>
          Your encrypted API keys and webhook tokens are protected by your passphrase.
          We can&apos;t reset it — only destroy the vault and start over.
        </p>
        <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 12 }}>
          Your meetings — recordings, transcripts, summaries, notes, tags — stay on disk and open
          normally. The speaker names already written into them stay too.
        </p>
        <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 12 }}>
          <strong>Enrolled voices are destroyed.</strong> Voiceprints are stored inside the vault,
          so a reset erases every one. New recordings will stop recognising anyone until you enroll
          each person again from a recording they speak in.
        </p>
        <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 12 }}>
          You&apos;ll re-enter your provider keys after the reset.
        </p>
        {sharedWith && (
          <p data-testid="unlock-reset-shared" style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 12 }}>
            The reset applies to this profile folder, so {sharedWith} loses the same keys and
            voices when it next opens the profile.
          </p>
        )}
        <label className="field-label" style={{ marginTop: 16, marginBottom: 6 }}>
          Type <code>RESET</code> to confirm:
        </label>
        <input
          type="text"
          data-testid="unlock-reset-text"
          value={confirmText}
          onChange={(e) => setConfirmText(e.target.value)}
          disabled={busy}
          placeholder="RESET"
          autoFocus
          style={{ display: 'block', width: 200 }}
        />
        {error && (
          <p className="meta meta--danger" style={{ marginTop: 12 }}>
            {error}
          </p>
        )}
        <div className="modal__actions" style={{ marginTop: 20 }}>
          <button
            data-testid="unlock-reset-confirm"
            className="btn btn--primary"
            onClick={reset}
            disabled={!canReset}
          >
            {busy ? 'Resetting…' : 'Reset vault and start over'}
          </button>
          <button data-testid="unlock-reset-cancel" className="btn" onClick={onCancel} disabled={busy}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
