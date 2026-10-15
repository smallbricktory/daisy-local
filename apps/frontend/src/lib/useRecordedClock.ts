// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { recordedSeconds } from './recordingState';
import { useVisibleInterval } from './useVisibleInterval';
import type { RecordingSnapshot } from '../tauri';

/**
 * The recording clock in whole seconds: the audio recorded, paused spans and
 * capture gaps excluded. It re-syncs on every snapshot (start, pause, resume,
 * and the backend's re-reading after a capture gap) and ticks once a second
 * while the snapshot is recording. A paused snapshot freezes it; a
 * null snapshot leaves the last value showing. The first render already
 * reads the snapshot, so a remount mid-recording shows the recorded time.
 */
export function useRecordedClock(snap: RecordingSnapshot | null): number {
  const [seconds, setSeconds] = useState(() => (snap ? recordedSeconds(snap, Date.now()) : 0));
  useEffect(() => {
    if (snap) setSeconds(recordedSeconds(snap, Date.now()));
  }, [snap]);
  useVisibleInterval(() => {
    if (snap) setSeconds(recordedSeconds(snap, Date.now()));
  }, 1000, snap?.state === 'recording');
  return seconds;
}
