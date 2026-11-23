// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { errStr, tauri } from '../tauri';
import { useContentKey } from '../lib/contentKey';
import {
  RECOVERY_DONE, RECOVERY_ENTRY_INTRO, RECOVERY_ENTRY_LABEL, RECOVERY_ENTRY_LATER, RECOVERY_ENTRY_SUBMIT, RECOVERY_ENTRY_TITLE,
  RECOVERY_ALREADY_OPEN, RECOVERY_INCORRECT, RECOVERY_INTRO, RECOVERY_PASSPHRASE_LABEL, RECOVERY_PASSPHRASE_PROMPT,
  RECOVERY_SAVED_LABEL, RECOVERY_SHOW, RECOVERY_SHOW_FAILED, RECOVERY_START_NEW, RECOVERY_START_NEW_BODY,
  RECOVERY_START_NEW_CANCEL, RECOVERY_START_NEW_CONFIRM, RECOVERY_START_NEW_FAILED, RECOVERY_START_NEW_TITLE, RECOVERY_TITLE,
} from '../encryptionCopy';

interface WordsProps {
  /** The words when the caller already holds them (the `words_ready`
   *  event). Null: the dialog fetches them — straight away in machine mode,
   *  after the passphrase otherwise. */
  words: string[] | null;
  onClose: () => void;
}

/** The recovery words as a bulleted list. Done is available once the person
 *  confirms they saved them, and tells the backend they were seen. */
export function RecoveryWords({ words: given, onClose }: WordsProps) {
  const [words, setWords] = useState<string[] | null>(given);
  const [machine, setMachine] = useState<boolean | null>(given ? false : null);
  const [pass, setPass] = useState('');
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (given) return;
    let live = true;
    void tauri.vaultKind().then(async (kind) => {
      if (!live) return;
      setMachine(kind === 'machine');
      if (kind !== 'machine') return;
      try {
        const w = await tauri.recoveryWordsShow('');
        if (live) setWords(w);
      } catch { if (live) setFailed(true); }
    }).catch(() => { if (live) setMachine(false); });
    return () => { live = false; };
  }, [given]);

  async function show() {
    if (busy || pass.length === 0) return;
    setBusy(true);
    setFailed(false);
    try { setWords(await tauri.recoveryWordsShow(pass)); }
    catch { setFailed(true); setPass(''); }
    finally { setBusy(false); }
  }

  async function done() {
    setBusy(true);
    try { await tauri.recoveryWordsAcknowledge(); } catch { /* the marker clears on the next confirmation */ }
    onClose();
  }

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" data-testid="recovery-words" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">{RECOVERY_TITLE}</div>
        {words ? (
          <>
            <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 8 }}>{RECOVERY_INTRO}</p>
            <ul style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', margin: '12px 0', paddingLeft: 24 }}>
              {words.map((w, i) => <li key={i} data-testid={`recovery-word-${i}`}>{w}</li>)}
            </ul>
            <label className="meta" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
              <input type="checkbox" data-testid="recovery-saved" checked={saved} onChange={(e) => setSaved(e.target.checked)} disabled={busy} />
              {RECOVERY_SAVED_LABEL}
            </label>
            <div className="modal__actions" style={{ marginTop: 20 }}>
              <button data-testid="recovery-done" className="btn btn--primary" onClick={() => void done()} disabled={!saved || busy}>
                {RECOVERY_DONE}
              </button>
            </div>
          </>
        ) : machine === false ? (
          <>
            <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 8 }}>{RECOVERY_PASSPHRASE_PROMPT}</p>
            <label className="field-label" style={{ marginTop: 12, marginBottom: 6 }}>{RECOVERY_PASSPHRASE_LABEL}</label>
            <input
              type="password"
              data-testid="recovery-passphrase"
              autoComplete="current-password"
              value={pass}
              onChange={(e) => setPass(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') void show(); }}
              disabled={busy}
              autoFocus
              style={{ display: 'block', width: '100%' }}
            />
            {failed && <p data-testid="recovery-show-error" className="meta meta--danger" style={{ marginTop: 12 }}>{RECOVERY_SHOW_FAILED}</p>}
            <div className="modal__actions" style={{ marginTop: 20 }}>
              <button data-testid="recovery-show" className="btn btn--primary" onClick={() => void show()} disabled={busy || pass.length === 0}>
                {RECOVERY_SHOW}
              </button>
              <button data-testid="recovery-cancel" className="btn" onClick={onClose} disabled={busy}>Cancel</button>
            </div>
          </>
        ) : (
          <>
            {failed && <p className="meta meta--danger" style={{ marginTop: 8 }}>{RECOVERY_SHOW_FAILED}</p>}
            <div className="modal__actions" style={{ marginTop: 20 }}>
              <button data-testid="recovery-close" className="btn" onClick={onClose}>Close</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/** Full-screen screen for a vault that opened while no content key did:
 *  the library stays locked until recovery words reopen a key.
 *  `onDismiss` leaves it for the rest of the app, which records as before. */
export function RecoveryKeyEntry({ onRestored, onDismiss }: { onRestored: () => void; onDismiss: () => void }) {
  const [words, setWords] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [fresh, setFresh] = useState<string[] | null>(null);
  // A new key is offered only while no key of the profile opens.
  const unopenable = useContentKey() === 'unopenable';

  async function startNew() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try { setFresh(await tauri.contentKeyStartNew()); }
    catch { setError(RECOVERY_START_NEW_FAILED); setConfirming(false); }
    finally { setBusy(false); }
  }

  async function submit() {
    if (busy || words.trim().length === 0) return;
    setBusy(true);
    setError(null);
    try {
      await tauri.recoveryWordsRestore(words);
      onRestored();
    } catch (e) {
      setError(errStr(e).includes('already open') ? RECOVERY_ALREADY_OPEN : RECOVERY_INCORRECT);
      setBusy(false);
    }
  }

  if (fresh) return <RecoveryWords words={fresh} onClose={onRestored} />;

  return (
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflowY: 'auto' }}>
      <div className="hero" data-testid="recovery-entry" style={{ maxWidth: 520, width: '100%', margin: 'auto', padding: '24px var(--space-32)' }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 16 }}>
          <div className="hero-mark" aria-hidden="true" style={{ flexShrink: 0, margin: 0 }} />
          <h1 className="h1" style={{ margin: 0 }}>{RECOVERY_ENTRY_TITLE}</h1>
        </div>
        <p className="lede" style={{ marginTop: 12 }}>{RECOVERY_ENTRY_INTRO}</p>
        <section className="surface" style={{ marginTop: 20 }}>
          <label className="field-label" style={{ marginBottom: 6 }}>{RECOVERY_ENTRY_LABEL}</label>
          <input
            type="text"
            data-testid="recovery-entry-input"
            value={words}
            onChange={(e) => setWords(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter') void submit(); }}
            disabled={busy}
            autoFocus
            autoCapitalize="none"
            autoCorrect="off"
            spellCheck={false}
            style={{ display: 'block', width: '100%' }}
          />
          <div className="modal__actions" style={{ marginTop: 20 }}>
            <button data-testid="recovery-entry-later" className="btn" onClick={onDismiss} disabled={busy}>
              {RECOVERY_ENTRY_LATER}
            </button>
            <button data-testid="recovery-entry-submit" className="btn btn--primary" onClick={() => void submit()} disabled={busy || words.trim().length === 0}>
              {RECOVERY_ENTRY_SUBMIT}
            </button>
          </div>
          {error && <p data-testid="recovery-entry-error" className="meta meta--danger" style={{ marginTop: 16 }}>{error}</p>}
        </section>
        {unopenable && (confirming ? (
          <section className="surface" data-testid="recovery-start-new-confirm" style={{ marginTop: 20 }}>
            <div className="modal__title">{RECOVERY_START_NEW_TITLE}</div>
            <p style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 8 }}>{RECOVERY_START_NEW_BODY}</p>
            <div className="modal__actions" style={{ marginTop: 20 }}>
              <button data-testid="recovery-start-new-cancel" className="btn" onClick={() => setConfirming(false)} disabled={busy}>
                {RECOVERY_START_NEW_CANCEL}
              </button>
              <button data-testid="recovery-start-new-go" className="btn btn--primary" onClick={() => void startNew()} disabled={busy}>
                {RECOVERY_START_NEW_CONFIRM}
              </button>
            </div>
          </section>
        ) : (
          <p style={{ marginTop: 20 }}>
            <button data-testid="recovery-start-new" className="btn" onClick={() => setConfirming(true)} disabled={busy}>
              {RECOVERY_START_NEW}
            </button>
          </p>
        ))}
      </div>
    </div>
  );
}
