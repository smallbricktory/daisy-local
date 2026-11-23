// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useRef } from 'react';

export interface PageHeaderProps {
  title: React.ReactNode;
  /** One line. Longer help belongs next to the control it explains. */
  subtitle?: React.ReactNode;
  /** Pinned to the right edge of the title row. */
  actions?: React.ReactNode;
  /** A row beneath the title — filter chips, a scope switch. */
  filters?: React.ReactNode;
}

/** The band at the top of every route inside the rail. Sticks to the top of
 *  its page while the body scrolls under it, and publishes its own height on
 *  the page as --page-header-h, so anything else that sticks in the same
 *  scroller lands under the band instead of behind it. */
export function PageHeader({ title, subtitle, actions, filters }: PageHeaderProps) {
  const ref = useRef<HTMLElement>(null);
  useEffect(() => {
    const header = ref.current;
    const page = header?.parentElement;
    if (!header || !page) return;
    // The band is full-width, so the observer fires on every frame of a window
    // resize. The custom property inherits, so writing it re-resolves every
    // calc in the page's subtree — only a changed height is worth that.
    let last = -1;
    const publish = (h: number) => {
      const px = Math.round(h);
      if (px === last) return;
      last = px;
      page.style.setProperty('--page-header-h', `${px}px`);
    };
    publish(header.getBoundingClientRect().height);
    const ro = new ResizeObserver(([entry]) => {
      publish(entry.borderBoxSize?.[0]?.blockSize ?? entry.target.getBoundingClientRect().height);
    });
    ro.observe(header, { box: 'border-box' });
    return () => ro.disconnect();
  }, []);

  return (
    <header className="page-header" ref={ref}>
      <div className="page-header__bar">
        <h1 className="page-header__title">{title}</h1>
        {actions && <div className="page-header__actions">{actions}</div>}
      </div>
      {subtitle && <p className="page-header__subtitle">{subtitle}</p>}
      {filters && <div className="page-header__filters">{filters}</div>}
    </header>
  );
}
