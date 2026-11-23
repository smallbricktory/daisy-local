// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE

/** A setting that takes effect the moment it is flipped.
 *
 *  The rule the app follows: a switch is a setting that applies immediately —
 *  flipping it writes. A checkbox is one of several values a form collects and
 *  a Save or Continue button commits; it also carries agreement ("I have read
 *  …") and reveal ("Show passphrase"), neither of which is a setting. */
export function Toggle({ checked, onChange, label, testId, disabled }: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  testId: string;
  disabled?: boolean;
}) {
  return (
    <button
      data-testid={testId}
      type="button"
      role="switch"
      className="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
    />
  );
}
