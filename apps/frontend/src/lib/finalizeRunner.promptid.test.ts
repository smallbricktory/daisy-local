import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('../tauri', () => ({
  tauri: {
    summaryRegenerate: vi.fn().mockResolvedValue({}),
    recordingFinalizeAndSummarize: vi.fn(),
  },
  transcriptionProviderStatus: vi.fn(),
}));
vi.mock('./diarizeBus', () => ({ runDiarize: vi.fn() }));
vi.mock('./toastStore', () => ({ pushToast: vi.fn(), dismissToast: vi.fn() }));
vi.mock('./gatewayNotice', () => ({ showGatewayNoticeIfNeeded: vi.fn(() => false) }));

import { runFinalize } from './finalizeRunner';
import { tauri } from '../tauri';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('runFinalize regen-summary promptId threading', () => {
  it('passes the chosen promptId through to summaryRegenerate', async () => {
    await runFinalize('sess-1', 'regen-summary', true, 'builtin:zoom');
    expect(tauri.summaryRegenerate).toHaveBeenCalledWith('sess-1', 'builtin:zoom');
  });

  it('omits the promptId when none chosen (global default applies)', async () => {
    await runFinalize('sess-2', 'regen-summary', true);
    expect(tauri.summaryRegenerate).toHaveBeenCalledWith('sess-2', undefined);
  });
});
