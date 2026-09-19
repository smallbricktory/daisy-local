// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useRef, useState } from 'react';

export interface ConfirmDialogProps {
  title: string;
  body: React.ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  danger?: boolean;
  /** When set, user must type this exact string before confirm enables. */
  typedConfirm?: string;
  /** Renders above all other modals. The imperative global confirm uses
   *  this; a confirmation triggered from inside a stacked modal (e.g. the
   *  speaker labeler at z-index 1100) sits on top of it. */
  elevated?: boolean;
  onCancel: () => void;
  onConfirm: () => Promise<void> | void;
}

export function ConfirmDialog({
  title,
  body,
  confirmLabel,
  cancelLabel = 'Cancel',
  danger = false,
  typedConfirm,
  elevated = false,
  onCancel,
  onConfirm,
}: ConfirmDialogProps) {
  const [typed, setTyped] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const confirmBtnRef = useRef<HTMLButtonElement>(null);

  const typedOk = typedConfirm ? typed === typedConfirm : true;
  const canConfirm = typedOk && !busy;

  useEffect(() => {
    if (!typedConfirm) confirmBtnRef.current?.focus();
  }, [typedConfirm]);

  async function run() {
    if (!canConfirm) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
    } catch (e: unknown) {
      setError(String((e as { message?: unknown })?.message ?? e));
      setBusy(false);
    }
  }

  return (
    <div
      data-testid="confirm-backdrop"
      className={elevated ? 'modal-backdrop modal-backdrop--elevated' : 'modal-backdrop'}
      onClick={busy ? undefined : onCancel}
    >
      <div data-testid="confirm-modal" className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">{title}</div>
        <div style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 8 }}>{body}</div>

        {typedConfirm && (
          <>
            <label className="field-label" style={{ marginTop: 16, marginBottom: 6 }}>
              Type <code>{typedConfirm}</code> to confirm:
            </label>
            <input
              data-testid="confirm-text"
              type="text"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
              disabled={busy}
              placeholder={typedConfirm}
              autoFocus
              style={{ display: 'block', width: 200 }}
            />
          </>
        )}

        {error && (
          <p className="meta meta--danger" style={{ marginTop: 12 }}>
            {error}
          </p>
        )}

        <div className="modal__actions" style={{ marginTop: 20 }}>
          <button
            data-testid="confirm-accept"
            ref={confirmBtnRef}
            className={danger ? 'btn btn--danger-solid' : 'btn btn--primary'}
            onClick={run}
            disabled={!canConfirm}
          >
            {busy ? 'Working…' : confirmLabel}
          </button>
          {cancelLabel && (
            <button data-testid="confirm-cancel" className="btn" onClick={onCancel} disabled={busy}>
              {cancelLabel}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
