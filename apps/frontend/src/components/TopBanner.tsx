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
  // Literal class names, not `banner--${kind}`. `info` is the base look and
  // adds no modifier.
  const cls = {
    info: 'banner',
    warning: 'banner banner--warning',
    danger: 'banner banner--danger',
  }[kind];
  return (
    <div className={cls} role={kind === 'info' ? undefined : 'alert'}>
      <span className="banner__text">{children}</span>
      {actions}
      {onDismiss && (
        <button
          type="button"
          className="banner-dismiss"
          aria-label="Dismiss"
          onClick={onDismiss}
        >×</button>
      )}
    </div>
  );
}
