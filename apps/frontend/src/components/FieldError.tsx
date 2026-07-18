// Shared inline error pill.

interface Props {
  children?: React.ReactNode;
  /** Optional className extending the default `.meta`. */
  className?: string;
  style?: React.CSSProperties;
}

/** Renders a small red error message when children is truthy, else null —
 *  call sites can drop the surrounding `{err && …}`. */
export function FieldError({ children, className = '', style }: Props) {
  if (!children) return null;
  return (
    <p className={`meta ${className}`} style={{ color: 'var(--danger)', margin: '6px 0 0', ...style }}>
      {children}
    </p>
  );
}

/** Inline success/ok pill. Same shape, green. */
export function FieldOk({ children, className = '', style }: Props) {
  if (!children) return null;
  return (
    <p className={`meta ${className}`} style={{ color: 'var(--ok, #2d6a4f)', margin: '6px 0 0', ...style }}>
      {children}
    </p>
  );
}
