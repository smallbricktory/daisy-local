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
      className="modal-backdrop"
      style={elevated ? { zIndex: 3000 } : undefined}
      onClick={busy ? undefined : onCancel}
    >
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__title">{title}</div>
        <div style={{ fontSize: 14, lineHeight: 1.5, marginTop: 8 }}>{body}</div>

        {typedConfirm && (
          <>
            <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginTop: 16, marginBottom: 6 }}>
              Type <code>{typedConfirm}</code> to confirm:
            </label>
            <input
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
          <p className="meta" style={{ color: 'var(--danger)', marginTop: 12 }}>
            {error}
          </p>
        )}

        <div className="modal__actions" style={{ marginTop: 20 }}>
          <button
            ref={confirmBtnRef}
            className="btn btn--primary"
            onClick={run}
            disabled={!canConfirm}
            style={
              danger
                ? {
                    background: canConfirm ? 'var(--danger)' : undefined,
                    borderColor: canConfirm ? 'var(--danger)' : undefined,
                    opacity: canConfirm ? 1 : 0.55,
                  }
                : undefined
            }
          >
            {busy ? 'Working…' : confirmLabel}
          </button>
          {cancelLabel && (
            <button className="btn" onClick={onCancel} disabled={busy}>
              {cancelLabel}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
