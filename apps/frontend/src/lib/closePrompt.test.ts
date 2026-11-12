// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('../tauri', () => ({
  tauri: { resolveClosePrompt: vi.fn(() => Promise.resolve()) },
}));

import { tauri } from '../tauri';
import { askCloseChoice } from './closePrompt';
import { resolvePending } from './confirm';

beforeEach(() => vi.clearAllMocks());

describe('askCloseChoice', () => {
  it.each([
    ['confirm', 'minimize'],
    ['alternate', 'quit'],
    ['cancel', 'cancel'],
  ] as const)('%s answers %s', async (result, choice) => {
    const asked = askCloseChoice();
    resolvePending(result);
    await asked;
    expect(tauri.resolveClosePrompt).toHaveBeenCalledWith(choice);
  });
});
