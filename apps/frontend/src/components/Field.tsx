// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { ReactNode } from 'react';

/** A label over the control it names. Pass `htmlFor` to bind it to a control
 *  that carries the matching `id`. `title` shows as the label's tooltip. */
export function Field({ label, htmlFor, title, children }: {
  label: string;
  htmlFor?: string;
  title?: string;
  children: ReactNode;
}) {
  return (
    <div className="field">
      <label className="field-label" htmlFor={htmlFor} title={title}>{label}</label>
      {children}
    </div>
  );
}
