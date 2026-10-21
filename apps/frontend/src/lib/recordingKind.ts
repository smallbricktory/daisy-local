// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import type { RecordingKind } from '../tauri';

/** Chip text and hover title for how a session's audio reached this profile. */
export function recordingKindLabel(kind: RecordingKind): { chip: string; title: string } {
  return kind === 'phone_room'
    ? { chip: 'SYNC', title: 'Recorded on a device that syncs with this computer' }
    : { chip: 'IMPORTED', title: 'Imported from an audio file' };
}
