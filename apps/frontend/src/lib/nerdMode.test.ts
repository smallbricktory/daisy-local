// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';

vi.mock('../tauri', () => ({
  tauri: {
    machineSettings: vi.fn().mockResolvedValue({
      machine: 'test-machine',
      settings: { ui_zoom: 1, nerd_mode: true },
    }),
    updateMachineSettings: vi.fn().mockResolvedValue(undefined),
  },
}));

import { tauri } from '../tauri';
import {
  providersPath, routeFallback, sectionFallback, setNerdMode, useNerdMode,
  visibleNavKeys, visibleSections,
} from './nerdMode';

describe('visibleNavKeys', () => {
  it('drops Workflows, Analyzer and History when Nerd Mode is off', () => {
    expect(visibleNavKeys(false, true)).toEqual(['record', 'library', 'calendar', 'search', 'help']);
  });

  it('lists every rail item when Nerd Mode is on', () => {
    expect(visibleNavKeys(true, true)).toEqual([
      'record', 'library', 'calendar', 'search', 'tasks', 'analyzer', 'history', 'help',
    ]);
  });

  it('drops Calendar until a subscription exists, in either mode', () => {
    expect(visibleNavKeys(true, false)).not.toContain('calendar');
    expect(visibleNavKeys(false, false)).not.toContain('calendar');
  });
});

describe('visibleSections', () => {
  it('drops Integrations, Voiceprints and MCP when Nerd Mode is off', () => {
    expect(visibleSections(false)).toEqual([
      'Display', 'Recordings', 'Tags', 'Calendars', 'Mobile Sync', 'Prompts', 'Providers',
      'Storage', 'Profile', 'About',
    ]);
  });

  it('lists every section when Nerd Mode is on', () => {
    expect(visibleSections(true)).toHaveLength(13);
  });
});

describe('fallbacks', () => {
  it('sends a hidden route to Library and leaves a visible one alone', () => {
    expect(routeFallback(false, 'analyzer')).toBe('library');
    expect(routeFallback(false, 'workflows')).toBe('library');
    expect(routeFallback(false, 'history')).toBe('library');
    expect(routeFallback(false, 'search')).toBeNull();
    expect(routeFallback(true, 'analyzer')).toBeNull();
  });

  it('sends a hidden section to Recordings and leaves a visible one alone', () => {
    expect(sectionFallback(false, 'Integrations')).toBe('Recordings');
    expect(sectionFallback(false, 'MCP')).toBe('Recordings');
    expect(sectionFallback(false, 'Display')).toBeNull();
    expect(sectionFallback(false, 'Providers')).toBeNull();
    expect(sectionFallback(true, 'MCP')).toBeNull();
  });
});

describe('providersPath', () => {
  it('names the Providers tab', () => {
    expect(providersPath()).toBe('Settings → Providers');
  });
});

describe('the store', () => {
  beforeEach(() => { vi.clearAllMocks(); });

  it('seeds from this machine settings', async () => {
    const { result } = renderHook(() => useNerdMode());
    await waitFor(() => expect(result.current).toBe(true));
  });

  it('writes only the flag through updateMachineSettings', async () => {
    const { result } = renderHook(() => useNerdMode());
    await waitFor(() => expect(result.current).toBe(true));
    await act(async () => { await setNerdMode(false); });
    expect(tauri.updateMachineSettings).toHaveBeenCalledWith({ nerd_mode: false });
    expect(result.current).toBe(false);
  });

  it('puts the switch back when the write fails', async () => {
    const { result } = renderHook(() => useNerdMode());
    await act(async () => { await setNerdMode(true).catch(() => {}); });
    vi.mocked(tauri.updateMachineSettings).mockRejectedValueOnce(new Error('vault locked'));
    await act(async () => {
      await expect(setNerdMode(false)).rejects.toThrow('vault locked');
    });
    expect(tauri.updateMachineSettings).toHaveBeenCalledWith({ nerd_mode: false });
    expect(result.current).toBe(true);
  });
});
