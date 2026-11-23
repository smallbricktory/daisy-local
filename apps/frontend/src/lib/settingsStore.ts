// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The profile's settings and this machine's settings block, each mirrored
// from the backend, which announces every write from any window or command
// (`settings:changed`, `machine-settings:changed`). A write sends only the
// fields it changes, so two screens writing different fields never undo each
// other.

import { backendStore } from './backendStore';
import {
  tauri,
  type MachinePatch,
  type MachineSettings,
  type MachineSettingsStatus,
  type Settings,
  type SettingsPatch,
} from '../tauri';

export const machineStore = backendStore<MachineSettingsStatus | null>(
  'machine-settings:changed', () => tauri.machineSettings(), null,
);

export const settingsStore = backendStore<Settings | null>(
  'settings:changed', () => tauri.readSettings(), null,
);

/** This machine's settings block, or null until the first answer. */
export function useMachineSettings(): MachineSettings | null {
  return machineStore.use()?.settings ?? null;
}

/** The machine name the block is stored under, empty until the first answer. */
export function useMachineName(): string {
  return machineStore.use()?.machine ?? '';
}

/** The profile's settings, or null until the first answer. */
export function useSettings(): Settings | null {
  return settingsStore.use();
}

/** Stores the fields `patch` carries on this machine. Every view shows them at
 *  once; a refused write reads the stored block back and rethrows. */
export async function setMachine(patch: MachinePatch): Promise<void> {
  const cur = machineStore.get();
  if (cur) machineStore.set({ ...cur, settings: { ...cur.settings, ...patch } });
  try {
    await tauri.updateMachineSettings(patch);
  } catch (e) {
    await machineStore.refresh().catch(() => {});
    throw e;
  }
}

/** Stores the fields `patch` carries in the profile's settings. Every view
 *  shows them at once; a refused write reads the stored settings back and
 *  rethrows. */
export async function setSettings(patch: SettingsPatch): Promise<void> {
  const cur = settingsStore.get();
  if (cur) settingsStore.set({ ...cur, ...patch });
  try {
    await tauri.updateSettings(patch);
  } catch (e) {
    await settingsStore.refresh().catch(() => {});
    throw e;
  }
}
