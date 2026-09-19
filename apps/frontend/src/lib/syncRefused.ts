// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The words for a phone session this computer cannot open, shared by the
// Mobile Sync panel and the toast the `sync:refused` event raises.

import type { RefusalReason, SyncRefused } from '../tauri';
import type { ToastSpec } from './toastStore';

/** What is wrong, as the tail of "<n> recordings from <device> …". */
export function refusalCause(reason: RefusalReason): string {
  if (reason === 'sealed') return 'sealed to a key this computer does not hold';
  return 'made with a newer Daisy than this one';
}

/** What makes them arrive. */
export function refusalRemedy(reason: RefusalReason): string {
  if (reason === 'sealed') return 'Unpair the device and pair it again to sync them.';
  return 'Update Daisy on this computer to sync them.';
}

export function syncRefusedToast(ev: SyncRefused): ToastSpec {
  return {
    id: `sync-refused-${ev.session}`,
    severity: 'warning',
    title: 'A recording could not be opened',
    body: `It is ${refusalCause(ev.reason)}. ${refusalRemedy(ev.reason)} Settings → Mobile Sync lists it.`,
  };
}
