// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The "paused while the computer slept" notice: live as a toast and a
// transcript marker (`recording:slept`), and on the session page from the
// manifest's recording segments.

import type { RecordingSlept } from '../tauri';
import { fmtTime } from './format';
import type { ToastSpec } from './toastStore';

/** Manifest `recording_segments[].reason` of a segment opened on wake. */
const SYSTEM_SLEEP_REASON = 'system_sleep';

export interface SegmentLoose {
  started_at_unix_seconds?: number;
  stopped_at_unix_seconds?: number | null;
  reason?: string | null;
}

export function sleepNoticeText(fromUnix: number, toUnix: number): string {
  return `Recording paused while the computer slept (${fmtTime(fromUnix)}–${fmtTime(toUnix)})`;
}

/** One notice per segment opened after a system sleep, spanning the
 *  previous segment's stop to this segment's start. */
export function sleepNotices(segments: SegmentLoose[] | null | undefined): string[] {
  const out: string[] = [];
  const segs = segments ?? [];
  for (let i = 1; i < segs.length; i++) {
    const seg = segs[i];
    const from = segs[i - 1].stopped_at_unix_seconds;
    const to = seg.started_at_unix_seconds;
    if (seg.reason !== SYSTEM_SLEEP_REASON || from == null || to == null) continue;
    out.push(sleepNoticeText(from, to));
  }
  return out;
}

export function sleptToast(s: RecordingSlept): ToastSpec {
  const title = sleepNoticeText(s.paused_at_unix_seconds, s.resumed_at_unix_seconds);
  return s.resumed
    ? {
        id: 'recording-slept',
        severity: 'info',
        title,
        body: 'Recording resumed when the computer woke.',
      }
    : {
        id: 'recording-slept',
        severity: 'warning',
        title,
        body: 'The recording could not restart. Press Resume to continue.',
      };
}
