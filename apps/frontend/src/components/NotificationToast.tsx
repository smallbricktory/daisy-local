// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Single corner-anchored stack that renders every transient notification.
// Pulls state from `lib/toastStore`. One shape for every toast: severity
// changes the rule strip and dot, never the structure or the controls.

import { useToasts, dismissToast, type ToastSpec } from '../lib/toastStore';
import { slugify } from '../lib/format';

export function ToastStack() {
  const toasts = useToasts();
  if (toasts.length === 0) return null;
  return (
    <div className="bgfin-stack" role="status" aria-live="polite">
      {toasts.map((t) => <ToastView key={t.id} t={t} />)}
    </div>
  );
}

function ToastView({ t }: { t: ToastSpec }) {
  const sevClass = severityClass(t.severity);
  const clickable = !!t.onClick;
  const body = (
    <>
      <span className={`bgfin-dot ${sevClass}`} aria-hidden="true" />
      <div style={{ display: 'flex', flexDirection: 'column', minWidth: 0, flex: 1 }}>
        <span className="bgfin-text">{t.title}</span>
        {t.body && (
          <span className="bgfin-text meta" style={{ fontSize: 'var(--t-meta)', marginTop: 2 }}>
            {t.body}
          </span>
        )}
        {t.timings && (
          <span className="bgfin-detail" style={{ marginTop: 4 }}>
            {t.timings}
          </span>
        )}
        {(t.progress != null || t.severity === 'working') && (
          <div className="bgfin-toast__track">
            <div
              className={t.progress == null ? 'bgfin-toast__bar bgfin-toast__bar--indeterminate' : 'bgfin-toast__bar'}
              style={{
                width: t.progress != null ? `${Math.max(0, Math.min(1, t.progress)) * 100}%` : undefined,
              }}
            />
          </div>
        )}
        {(t.actions && t.actions.length > 0) && (
          <div className="bgfin-actions" style={{ marginTop: 6 }}>
            {t.actions.map((a, i) => (
              <button
                key={i}
                data-testid={`toast-action-${slugify(a.label)}`}
                type="button"
                className="bgfin-action"
                onClick={(e) => { e.stopPropagation(); a.onClick(); }}
                style={a.primary ? { background: 'var(--gold)', color: 'var(--on-gold)', borderColor: 'var(--gold)' } : undefined}
              >
                {a.label}
              </button>
            ))}
          </div>
        )}
      </div>
      <button
        data-testid="toast-dismiss"
        type="button"
        className="toast-dismiss"
        aria-label="Dismiss"
        onClick={(e) => { e.stopPropagation(); dismissToast(t.id, true); }}
      >×</button>
    </>
  );
  return (
    <div
      data-testid={`toast-${t.id}`}
      className={clickable ? `bgfin-toast ${sevClass} bgfin-toast--clickable` : `bgfin-toast ${sevClass}`}
      onClick={t.onClick}
    >
      {body}
    </div>
  );
}

function severityClass(s: ToastSpec['severity']): string {
  switch (s) {
    case 'working': return 'bgfin-toast--working';
    case 'done':    return 'bgfin-toast--done';
    case 'warning': return 'bgfin-toast--stuck';
    case 'error':   return 'bgfin-toast--stuck';
    default:        return 'bgfin-toast--working';
  }
}
