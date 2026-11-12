// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The words for a phone session this computer cannot open, shared by the
// Mobile Sync panel and the toast the `sync:refused` event raises.

import type { RefusalReason, SyncRefused } from '../tauri';
import { fmtDateTimeYear } from './format';
import type { ToastSpec } from './toastStore';

/** What is wrong, as the tail of "<n> recordings from <device> …". */
export function refusalCause(reason: RefusalReason): string {
  switch (reason) {
    case 'sealed': return 'sealed to a key this computer does not hold';
    case 'unsupported': return 'made with a newer Daisy than this one';
    case 'undecodable': return 'missing playable audio';
  }
}

/** What makes `count` recordings arrive, or clears them. */
export function refusalRemedy(reason: RefusalReason, count: number): string {
  const them = count === 1 ? 'it' : 'them';
  switch (reason) {
    case 'sealed': return `Unpair the device and pair it again to sync ${them}.`;
    case 'unsupported': return `Update Daisy on this computer to sync ${them}.`;
    case 'undecodable': return `Delete ${them} on the phone to clear ${them}.`;
  }
}

/** The recording by its title and start time, as far as they are known. */
function recordingName(ev: SyncRefused): string {
  const when = ev.recorded_at_unix_seconds === null ? null : fmtDateTimeYear(ev.recorded_at_unix_seconds);
  if (ev.title && when) return `“${ev.title}” from ${when}`;
  if (ev.title) return `“${ev.title}”`;
  if (when) return `The recording from ${when}`;
  return 'It';
}

export function syncRefusedToast(ev: SyncRefused): ToastSpec {
  return {
    id: `sync-refused-${ev.session}`,
    severity: 'warning',
    title: 'A recording could not be opened',
    body: `${recordingName(ev)} is ${refusalCause(ev.reason)}. ${refusalRemedy(ev.reason, 1)} Settings → Mobile Sync lists it.`,
  };
}
