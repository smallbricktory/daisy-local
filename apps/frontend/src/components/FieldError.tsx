// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Shared inline error pill.

interface Props {
  children?: React.ReactNode;
  /** Optional className extending the default `.meta`. */
  className?: string;
  style?: React.CSSProperties;
  /** Test id for the rendered message. */
  testId?: string;
}

/** Renders a small red error message when children is truthy, else null —
 *  call sites can drop the surrounding `{err && …}`. */
export function FieldError({ children, className = '', style, testId }: Props) {
  if (!children) return null;
  return (
    <p data-testid={testId} className={`meta meta--danger ${className}`} style={{ margin: '6px 0 0', ...style }}>
      {children}
    </p>
  );
}

/** Inline success/ok pill. Same shape, green. */
export function FieldOk({ children, className = '', style }: Props) {
  if (!children) return null;
  return (
    <p className={`meta meta--success ${className}`} style={{ margin: '6px 0 0', ...style }}>
      {children}
    </p>
  );
}
