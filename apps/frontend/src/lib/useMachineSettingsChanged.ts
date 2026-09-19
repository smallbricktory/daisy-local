// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useBackendEvent } from './useBackendEvent';

/** Runs `handler` whenever the backend announces a machine-settings change
 * made outside the Settings pane (`machine-settings:changed`), such as the
 * meeting-prompt popup writing a snooze or "Never ask". The event carries no
 * payload; callers re-fetch the machine block in the handler. */
export function useMachineSettingsChanged(handler: () => void): void {
  useBackendEvent('machine-settings:changed', handler);
}
