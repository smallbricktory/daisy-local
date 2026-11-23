// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useBackendEvent } from './useBackendEvent';

/** Runs `handler` whenever the backend announces a device-topology change
 * (`audio-sources-changed`). Callers re-fetch their source list in the
 * handler; the event carries no payload. */
export function useAudioSourcesChanged(handler: () => void): void {
  useBackendEvent('audio-sources-changed', handler);
}
