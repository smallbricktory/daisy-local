// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { scanInvokes } from './invoke-scan';

describe('scanInvokes', () => {
  it('finds plain and generic calls', () => {
    const s = "await invoke('read_session'); invoke<string>('eula_status')";
    expect(scanInvokes(s).map((x) => x.name)).toEqual(['read_session', 'eula_status']);
  });

  it('crosses a nested generic argument', () => {
    const s = "invoke<Record<string, number>>('workflow_history_read')";
    expect(scanInvokes(s).map((x) => x.name)).toEqual(['workflow_history_read']);
  });

  it('ignores calls whose first argument is not a literal name', () => {
    expect(scanInvokes('invoke(cmd)')).toEqual([]);
  });
});
