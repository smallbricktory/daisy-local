// Single corner-anchored stack that renders every transient notification.
// Pulls state from `lib/toastStore`. One shape for every toast: severity
// changes the rule strip and dot, never the structure or the controls.

import { useToasts, dismissToast, type ToastSpec } from '../lib/toastStore';

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
          <span className="bgfin-text" style={{ fontSize: 11, color: 'var(--text-dim)', marginTop: 2 }}>
            {t.body}
          </span>
        )}
        {t.timings && (
          <span style={{ fontFamily: 'var(--font-mono)', fontSize: 10, color: 'var(--text-dim)', marginTop: 4, letterSpacing: '0.04em' }}>
            {t.timings}
          </span>
        )}
        {(t.progress != null || t.severity === 'working') && (
          <div style={{
            height: 4, marginTop: 6, borderRadius: 99, background: 'var(--line)', overflow: 'hidden',
          }}>
            <div
              className={t.progress == null ? 'bgfin-toast__bar--indeterminate' : undefined}
              style={{
                height: '100%',
                width: t.progress != null ? `${Math.max(0, Math.min(1, t.progress)) * 100}%` : undefined,
                background: 'var(--gold-text)',
                transition: t.progress != null ? 'width 200ms ease' : undefined,
              }}
            />
          </div>
        )}
        {(t.actions && t.actions.length > 0) && (
          <div className="bgfin-actions" style={{ marginTop: 6 }}>
            {t.actions.map((a, i) => (
              <button
                key={i}
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
        type="button"
        className="toast-dismiss"
        aria-label="Dismiss"
        onClick={(e) => { e.stopPropagation(); dismissToast(t.id); }}
      >×</button>
    </>
  );
  return (
    <div
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
