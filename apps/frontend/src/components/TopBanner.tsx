// Inline top-of-window banner. Used for license trial / expiry, update
// available, vault model missing, etc. — anything that's not transient
// enough for a corner toast but not blocking enough for a modal.

export type TopBannerKind = 'info' | 'warning' | 'danger';

interface Props {
  kind?: TopBannerKind;
  children: React.ReactNode;
  /** Optional dismiss button rendered on the right side. */
  onDismiss?: () => void;
  /** Optional inline actions (right-aligned, before dismiss). */
  actions?: React.ReactNode;
}

export function TopBanner({ kind = 'info', children, onDismiss, actions }: Props) {
  const palette = {
    info:    { bg: 'var(--tint, #f3edff)',    border: 'var(--frost-deep)', fg: 'var(--ink)' },
    warning: { bg: 'var(--amber, #FFF3CD)',   border: 'var(--frost-deep)', fg: 'var(--ink)' },
    danger:  { bg: '#ffe5e5',                  border: '#f5b3b3',          fg: '#5a1212' },
  }[kind];
  return (
    <div style={{
      background: palette.bg, borderBottom: `1px solid ${palette.border}`,
      color: palette.fg, padding: '8px 14px', fontSize: 13,
      display: 'flex', alignItems: 'center', gap: 10, flexWrap: 'wrap',
    }}>
      <span style={{ flex: 1, minWidth: 0 }}>{children}</span>
      {actions}
      {onDismiss && (
        <button
          type="button"
          className="banner-dismiss"
          aria-label="Dismiss"
          onClick={onDismiss}
          style={{
            background: 'transparent', border: 0, padding: '2px 6px',
            cursor: 'pointer', color: 'inherit', fontSize: 14, lineHeight: 1,
          }}
        >×</button>
      )}
    </div>
  );
}
