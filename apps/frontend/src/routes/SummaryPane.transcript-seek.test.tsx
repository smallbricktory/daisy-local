// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn() }));
vi.mock('../components/tags/TagChip', () => ({ TagChip: () => null }));
vi.mock('../components/tags/TagCombobox', () => ({ TagCombobox: () => null }));
vi.mock('../components/tags/TagPromptModal', () => ({ TagPromptModal: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <pre>{markdown}</pre>,
}));
vi.mock('../tauri', () => ({
  tauri: { builtinModelStatus: vi.fn(() => Promise.resolve({ model: null, provider: null })) },
  errStr: vi.fn((e: unknown) => String(e)),
  formatDurationHm: vi.fn(() => '—'),
  formatDurationSeconds: vi.fn(() => '—'),
  LANGUAGES: [],
  copyToClipboard: vi.fn(),
}));

import { TranscriptTab } from './SummaryPane';

/** Three turns of one speaker, as the renderer caps them, then a reply. */
const MD = [
  '[00:00:03] **Jordan Rivera**: Let us start with the roadmap.',
  '',
  '[00:01:10] **Jordan Rivera**: Onboarding comes first.',
  '',
  '[00:02:30] **Jordan Rivera**: The vendor migration follows.',
  '[00:03:05] **Me**: Agreed.',
].join('\n');

describe('TranscriptTab click-to-seek', () => {
  it('shows each rendered turn as its own block, same speaker included', () => {
    render(<TranscriptTab md={MD} onSeek={vi.fn()} />);
    expect(screen.getAllByTestId(/^transcript-turn-/)).toHaveLength(4);
    expect(screen.getByTestId('transcript-turn-1').textContent).toContain('00:01:10Onboarding comes first.');
  });

  it('seeks to the start of the turn that was clicked', () => {
    const onSeek = vi.fn();
    render(<TranscriptTab md={MD} onSeek={onSeek} />);
    fireEvent.click(screen.getByText('Onboarding comes first.', { exact: false }));
    expect(onSeek).toHaveBeenLastCalledWith(70);
    fireEvent.click(screen.getAllByText('Jordan Rivera')[2]);
    expect(onSeek).toHaveBeenLastCalledWith(150);
    fireEvent.click(screen.getByTestId('transcript-turn-3'));
    expect(onSeek).toHaveBeenLastCalledWith(185);
  });
});
