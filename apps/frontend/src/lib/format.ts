// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Cached Intl.DateTimeFormat instances — constructing a fresh formatter per
// call dominates the format cost on long lists. One instance per pattern,
// many .format() calls.
const DATE = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' });
const DATE_YEAR = new Intl.DateTimeFormat(undefined, {
  month: 'short', day: 'numeric', year: 'numeric',
});
const TIME = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const DATE_TIME = new Intl.DateTimeFormat(undefined, {
  month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit',
});
const DATE_TIME_YEAR = new Intl.DateTimeFormat(undefined, {
  year: 'numeric', month: 'short', day: 'numeric',
  hour: 'numeric', minute: '2-digit',
});
const WEEKDAY_DATE_TIME = new Intl.DateTimeFormat(undefined, {
  weekday: 'short', month: 'short', day: 'numeric',
  hour: 'numeric', minute: '2-digit',
});

/** "Jun 8" — day without year (recent-items lists). */
export function fmtDate(unixSeconds: number): string {
  return DATE.format(unixSeconds * 1000);
}
/** "Jun 8, 2026" — day with year (results that can be arbitrarily old). */
export function fmtDateYear(unixSeconds: number): string {
  return DATE_YEAR.format(unixSeconds * 1000);
}
/** "3:07 PM" — clock time alone. */
export function fmtTime(unixSeconds: number): string {
  return TIME.format(unixSeconds * 1000);
}
/** "Jun 8, 3:07 PM" — day + time without year. */
export function fmtDateTime(unixSeconds: number): string {
  return DATE_TIME.format(unixSeconds * 1000);
}
/** "Jun 8, 2026, 3:07 PM" — full timestamp. */
export function fmtDateTimeYear(unixSeconds: number): string {
  return DATE_TIME_YEAR.format(unixSeconds * 1000);
}
/** "Mon, Jun 8, 3:07 PM" — weekday + day + time (calendar rows). */
export function fmtWeekdayDateTime(unixSeconds: number): string {
  return WEEKDAY_DATE_TIME.format(unixSeconds * 1000);
}

/** Lowercase, runs of non-alphanumerics collapsed to `-`. Testids derived from
 *  labels use this, and the e2e page objects derive the same ids with it. */
export function slugify(s: string): string {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, '-');
}

/** Zero-padded HH:MM:SS from a duration in seconds. */
export function fmtHms(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`;
}
