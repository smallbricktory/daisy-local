// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Nerd Mode: this machine's "show every screen and setting" switch, and the
// lists the rail and the Settings tabs are filtered through. The switch is
// MachineSettings.nerd_mode, read and written through the machine store.
import { setMachine, useMachineSettings } from './settingsStore';

/** Rail keys, in NAV_DEFS order. App.tsx types NAV_DEFS against this. */
export const NAV_KEYS = [
  'record', 'library', 'calendar', 'search', 'tasks', 'analyzer', 'history', 'help',
] as const;
export type NavKey = (typeof NAV_KEYS)[number];

/** Settings tabs, in the order they are listed. */
export const SETTINGS_SECTIONS = [
  'Display', 'Recordings', 'Tags', 'Calendars', 'Mobile Sync', 'Prompts', 'Providers',
  'Integrations', 'Storage', 'Voiceprints', 'MCP', 'Profile', 'About',
] as const;
export type SettingsSection = (typeof SETTINGS_SECTIONS)[number];

const NERD_ONLY_NAV: readonly NavKey[] = ['tasks', 'analyzer', 'history'];
const NERD_ONLY_SECTIONS: readonly SettingsSection[] = ['Integrations', 'Voiceprints', 'MCP'];
const NERD_ONLY_ROUTES: readonly string[] = ['workflows', 'analyzer', 'history'];

/** The rail items on screen. Calendar also needs a subscription. */
export function visibleNavKeys(nerd: boolean, hasCalendar: boolean): NavKey[] {
  return NAV_KEYS.filter((k) => {
    if (k === 'calendar' && !hasCalendar) return false;
    return nerd || !NERD_ONLY_NAV.includes(k);
  });
}

/** The Settings tabs on screen. */
export function visibleSections(nerd: boolean): SettingsSection[] {
  return SETTINGS_SECTIONS.filter((s) => nerd || !NERD_ONLY_SECTIONS.includes(s));
}

/** Where a route that just became hidden goes; null while it stays visible. */
export function routeFallback(nerd: boolean, route: string): 'library' | null {
  return !nerd && NERD_ONLY_ROUTES.includes(route) ? 'library' : null;
}

/** Where a section that just became hidden goes; null while it stays visible. */
export function sectionFallback(nerd: boolean, section: string): 'Recordings' | null {
  return visibleSections(nerd).some((s) => s === section) ? null : 'Recordings';
}

/** Where the user finds Providers. */
export function providersPath(): string {
  return 'Settings → Providers';
}

/** The switch; off until this machine's settings first answer. */
export function useNerdMode(): boolean {
  return useMachineSettings()?.nerd_mode ?? false;
}

/** Flips the switch on this machine. A failed write puts it back. */
export function setNerdMode(v: boolean): Promise<void> {
  return setMachine({ nerd_mode: v });
}
