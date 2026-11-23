// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE

/** "Paused until 3:40 PM" while a snooze is still running, else null. */
export function snoozeHelp(snoozeUntil: number | null, nowMs: number): string | null {
  if (snoozeUntil === null) return null;
  const untilMs = snoozeUntil * 1000;
  if (untilMs <= nowMs) return null;
  const at = new Date(untilMs).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
  return `Paused until ${at}`;
}
