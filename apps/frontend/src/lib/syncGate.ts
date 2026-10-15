// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The words for a paired device refused at the version gate, shared by the
// Mobile Sync panel's device row and the toast the `sync:gated` event raises.

import type { OutOfDate, SyncGated } from '../tauri';
import type { ToastSpec } from './toastStore';

/** The device whose Daisy is out of date, as the sentence names it. */
function deviceNoun(outOfDate: OutOfDate, platform: string): string {
  if (outOfDate === 'desktop') return 'desktop';
  return platform === 'android' ? 'Android device' : 'iOS device';
}

export function outOfDateText(outOfDate: OutOfDate, platform: string): string {
  return `Your ${deviceNoun(outOfDate, platform)} is out of date. Please update it to complete the sync. Sync has NOT been done.`;
}

export function syncGatedToast(ev: SyncGated): ToastSpec {
  return {
    id: `sync-gated-${ev.device_id}`,
    severity: 'warning',
    title: 'Sync has not been done',
    body: outOfDateText(ev.out_of_date, ev.platform),
  };
}
