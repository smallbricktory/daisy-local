// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The two shapes every Settings pane is built from. A pane lives in this file's
// route or in a component of its own; both name their groups the same way.
import type { ReactNode } from 'react';
import { useNerdMode } from '../lib/nerdMode';

/** A titled group of related controls. `scope` marks settings that apply
 *  beyond this machine; the mark shows while Nerd Mode is on. */
export function Group({ title, hint, scope, children }: {
  title?: string; hint?: ReactNode; scope?: string; children: ReactNode;
}) {
  const nerd = useNerdMode();
  return (
    <section className="set-group">
      {title && (
        <h2 className="set-group__title">
          {title}
          {scope && nerd && <span className="set-group__scope">{scope}</span>}
        </h2>
      )}
      {hint && <p className="set-group__hint">{hint}</p>}
      {children}
    </section>
  );
}

/** One setting: what it is (and why) on the left, the control on the right. */
export function SettingRow({ label, help, stack, children }: {
  label: string; help?: ReactNode; stack?: boolean; children: ReactNode;
}) {
  return (
    <div className={`set-row${stack ? ' set-row--stack' : ''}`}>
      <div>
        <div className="set-row__label">{label}</div>
        {help && <p className="set-row__help">{help}</p>}
      </div>
      {children}
    </div>
  );
}
