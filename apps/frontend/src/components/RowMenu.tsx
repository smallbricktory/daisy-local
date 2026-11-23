// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { Fragment, useRef, useState } from 'react';
import { useDismiss } from '../lib/useDismiss';

export interface RowMenuItem {
  label: string;
  onSelect: () => void;
  /** Destructive: red inside the menu, and a confirm behind it. */
  danger?: boolean;
  disabled?: boolean;
  testId?: string;
  /** A separator is drawn above this item. */
  separated?: boolean;
}

export interface RowMenuProps {
  items: RowMenuItem[];
  /** Names the row this menu belongs to, e.g. "Delete workflow". */
  label?: string;
  disabled?: boolean;
  /** Kebab shows an ellipsis in place of the glyph while true. */
  busy?: boolean;
  /** The menu opens above the kebab, for a row at the bottom of the window. */
  up?: boolean;
  testId?: string;
}

/** The overflow a row's secondary and destructive actions live behind.
 *  Closes on outside click, on Escape, and after a selection. */
export function RowMenu({
  items,
  label = 'Row actions',
  disabled = false,
  busy = false,
  up = false,
  testId,
}: RowMenuProps) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);

  useDismiss(open, ref, () => setOpen(false));

  const shown = items.filter((i) => i.label);
  if (shown.length === 0) return null;

  return (
    <div className="row-menu" ref={ref}>
      <button
        data-testid={testId}
        type="button"
        className="row-menu__kebab"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        disabled={disabled || busy}
        onClick={() => setOpen((v) => !v)}
      >
        {busy ? '…' : '⋯'}
      </button>
      {open && (
        <div className={up ? 'menu row-menu__pop row-menu__pop--up' : 'menu row-menu__pop'} role="menu">
          {shown.map((item, i) => (
            <Fragment key={item.label}>
              {item.separated && i > 0 && <div className="menu__sep" role="separator" />}
              <button
                data-testid={item.testId}
                type="button"
                role="menuitem"
                className={item.danger ? 'menu__item menu__item--danger' : 'menu__item'}
                disabled={item.disabled}
                onClick={() => { setOpen(false); item.onSelect(); }}
              >
                {item.label}
              </button>
            </Fragment>
          ))}
        </div>
      )}
    </div>
  );
}
