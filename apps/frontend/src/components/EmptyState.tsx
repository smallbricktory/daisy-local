// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { ReactNode } from 'react';

/** What a container says while it holds nothing: what belongs here, and the
 *  one step that puts it there. `action` is omitted when the screen already
 *  carries that step — the panel's own button, the chat's own input. */
export function EmptyState({ title, children, action, testid, inset = false }: {
  title: string;
  children: ReactNode;
  action?: ReactNode;
  testid?: string;
  /** Sits inside a panel rather than filling a route. */
  inset?: boolean;
}) {
  return (
    <div className={inset ? 'empty empty--inset' : 'empty'} data-testid={testid} data-empty="">
      <p className="empty__title">{title}</p>
      <p className="empty__body">{children}</p>
      {action && <div className="empty__action">{action}</div>}
    </div>
  );
}
