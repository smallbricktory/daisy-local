// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, expect, it } from 'vitest';
import { exportFileName } from './exportFileName';

describe('exportFileName', () => {
  it('keeps letters of every script', () => {
    expect(exportFileName('Lunch with 이모', 'md')).toBe('Lunch_with_이모.md');
    expect(exportFileName('Café — revue', 'md')).toBe('Café_revue.md');
    expect(exportFileName('会議メモ 3', 'wav')).toBe('会議メモ_3.wav');
  });

  it('composes decomposed text before keeping it', () => {
    expect(exportFileName('Café', 'md')).toBe('Café.md');
  });

  it('replaces path and reserved characters', () => {
    expect(exportFileName('Q3/Q4: plan? <draft>', 'opus')).toBe('Q3_Q4_plan_draft.opus');
  });

  it('caps the name at 60 characters without splitting one', () => {
    const name = exportFileName('가'.repeat(80), 'md');
    expect(name).toBe(`${'가'.repeat(60)}.md`);
    expect(exportFileName('𠀀'.repeat(61), 'md')).toBe(`${'𠀀'.repeat(60)}.md`);
  });

  it('falls back to meeting when nothing is left', () => {
    expect(exportFileName('!!!', 'md')).toBe('meeting.md');
    expect(exportFileName('', 'wav')).toBe('meeting.wav');
  });
});
