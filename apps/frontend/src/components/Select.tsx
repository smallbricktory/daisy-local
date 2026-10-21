// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useId, useRef, useState } from 'react';
import { useDismiss } from '../lib/useDismiss';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/** A list of values, one of them chosen, drawn by the app.
 *
 *  The native control hands the list to the OS, which paints it in the system
 *  palette and the system metrics — a second design inside this one. This
 *  draws the list on the app's own menu surface and keeps the keyboard
 *  contract a listbox owes: arrows and Home/End move, typing jumps, Enter
 *  takes, Escape leaves it as it was. */
export function Select({
  value, options, onChange, onOpen, id, testId, label, disabled, className,
}: {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  /** Runs as the list opens — for a list whose contents are re-read each time
   *  it is shown (the microphones attached right now). */
  onOpen?: () => void;
  /** Binds a <label htmlFor> to the control. */
  id?: string;
  testId?: string;
  /** Names the control when no visible label does. */
  label?: string;
  disabled?: boolean;
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const typed = useRef<{ text: string; at: number }>({ text: '', at: 0 });
  const listId = useId();
  const selectedIndex = options.findIndex((o) => o.value === value);
  // A picker with nothing to offer is not a control: its list opens empty.
  const inert = disabled || options.length === 0;
  // The values themselves, so an effect can tell a list that changed from a
  // re-render that handed the same list back in a new array.
  const shape = JSON.stringify(options.map((o) => [o.value, !!o.disabled]));

  /** The next option in `delta`'s direction that can be chosen, or `from`. */
  function step(from: number, delta: number): number {
    for (let i = from + delta; i >= 0 && i < options.length; i += delta) {
      if (!options[i].disabled) return i;
    }
    return from >= 0 && from < options.length ? from : -1;
  }

  // A click outside leaves the list as it was and does not take the focus back
  // — the reader is already somewhere else. Escape is answered on the trigger
  // instead, where the key can be stopped from reaching an enclosing dialog.
  useDismiss(open, rootRef, () => setOpen(false), { escape: false });

  // Keep the active option in view as the arrows walk a long list.
  useEffect(() => {
    if (!open) return;
    listRef.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [open, active]);

  // An `onOpen` list is re-read while the list is already up, so the index the
  // arrows walk is re-derived from the values that arrived.
  useEffect(() => {
    if (open) setActive(selectedIndex >= 0 ? selectedIndex : step(-1, 1));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, shape, value]);

  function close() {
    setOpen(false);
    typed.current = { text: '', at: 0 };
    // WebKit does not focus a button on mousedown, so a mouse-opened list is
    // handed the focus its keyboard contract assumes — and handed it back when
    // the list goes, the way the native control does.
    triggerRef.current?.focus();
  }

  function openList() {
    if (inert) return;
    typed.current = { text: '', at: 0 };
    setActive(selectedIndex >= 0 ? selectedIndex : step(-1, 1));
    onOpen?.();
    setOpen(true);
    triggerRef.current?.focus();
  }

  function pick(i: number) {
    const o = options[i];
    close();
    if (!o || o.disabled) return;
    if (o.value !== value) onChange(o.value);
  }

  /** Typing letters jumps to the option that starts with them. */
  function jumpTo(ch: string) {
    const now = Date.now();
    typed.current = {
      text: now - typed.current.at > 900 ? ch : typed.current.text + ch,
      at: now,
    };
    const q = typed.current.text.toLowerCase();
    const from = open ? active : selectedIndex;
    const order = [...options.keys()].slice(from + 1).concat([...options.keys()].slice(0, from + 1));
    const hit = order.find((i) => !options[i].disabled && options[i].label.toLowerCase().startsWith(q));
    if (hit === undefined) return;
    if (open) setActive(hit); else pick(hit);
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLButtonElement>) {
    if (inert) return;
    if (e.key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey && e.key !== ' ') {
      e.preventDefault();
      jumpTo(e.key);
      return;
    }
    if (!open) {
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        openList();
      }
      return;
    }
    switch (e.key) {
      // The key belongs to the list while the list is up: an enclosing dialog
      // that also closes on Escape must not take it as well.
      case 'Escape': e.preventDefault(); e.stopPropagation(); close(); break;
      case 'Tab': setOpen(false); break;
      case 'Enter': case ' ': e.preventDefault(); pick(active); break;
      case 'ArrowDown': e.preventDefault(); setActive(step(active, 1)); break;
      case 'ArrowUp': e.preventDefault(); setActive(step(active, -1)); break;
      case 'Home': e.preventDefault(); setActive(step(-1, 1)); break;
      case 'End': e.preventDefault(); setActive(step(options.length, -1)); break;
      default: break;
    }
  }

  return (
    <div className={className ? `select ${className}` : 'select'} ref={rootRef}>
      <button
        ref={triggerRef}
        id={id}
        data-testid={testId}
        data-value={value}
        type="button"
        role="combobox"
        className="select__trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listId : undefined}
        aria-activedescendant={open && active >= 0 ? `${listId}-${active}` : undefined}
        aria-label={label}
        disabled={inert}
        onClick={() => (open ? close() : openList())}
        onKeyDown={onKeyDown}
      >
        <span className="select__value">{options[selectedIndex]?.label ?? ''}</span>
        <span className="select__caret" aria-hidden="true">▾</span>
      </button>
      {open && (
        <div className="menu select__pop" id={listId} role="listbox" ref={listRef}>
          {options.map((o, i) => (
            <div
              key={`${i}:${o.value}`}
              id={`${listId}-${i}`}
              role="option"
              data-value={o.value}
              data-active={i === active}
              aria-selected={i === selectedIndex}
              aria-disabled={o.disabled || undefined}
              className={i === selectedIndex ? 'menu__item select__option is-on' : 'menu__item select__option'}
              onMouseEnter={() => setActive(i)}
              // The trigger keeps the focus through the click, and a picker
              // inside a <label> must not have the click forwarded back to the
              // trigger as that label's own activation.
              onMouseDown={(e) => e.preventDefault()}
              onClick={(e) => { e.preventDefault(); pick(i); }}
            >
              {o.label}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
