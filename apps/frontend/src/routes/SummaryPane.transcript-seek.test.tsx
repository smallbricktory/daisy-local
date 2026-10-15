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

/** Three lines from one speaker read as one paragraph, then a reply. */
const MD = [
  '[00:00:03] **Jordan Rivera**: Let us start with the roadmap.',
  '[00:01:10] **Jordan Rivera**: Onboarding comes first.',
  '[00:02:30] **Jordan Rivera**: The vendor migration follows.',
  '[00:03:05] **Me**: Agreed.',
].join('\n');

describe('TranscriptTab click-to-seek on merged turns', () => {
  it('seeks to the timestamp of the line that was clicked', () => {
    const onSeek = vi.fn();
    render(<TranscriptTab md={MD} onSeek={onSeek} />);
    expect(screen.getAllByTestId(/^transcript-turn-/)).toHaveLength(2);

    fireEvent.click(screen.getByText('Onboarding comes first.', { exact: false }));
    expect(onSeek).toHaveBeenLastCalledWith(70);
    fireEvent.click(screen.getByText('The vendor migration follows.', { exact: false }));
    expect(onSeek).toHaveBeenLastCalledWith(150);
    fireEvent.click(screen.getByText('Let us start with the roadmap.'));
    expect(onSeek).toHaveBeenLastCalledWith(3);
  });

  it('a click outside the lines seeks to the turn start', () => {
    const onSeek = vi.fn();
    render(<TranscriptTab md={MD} onSeek={onSeek} />);
    fireEvent.click(screen.getByText('Jordan Rivera'));
    expect(onSeek).toHaveBeenLastCalledWith(3);
    fireEvent.click(screen.getByTestId('transcript-turn-1'));
    expect(onSeek).toHaveBeenLastCalledWith(185);
  });

  it('a line without a timestamp seeks to the line before it', () => {
    const onSeek = vi.fn();
    const md = ['[00:00:03] **Jordan Rivera**: First.', '**Jordan Rivera**: Untimed.'].join('\n');
    render(<TranscriptTab md={md} onSeek={onSeek} />);
    fireEvent.click(screen.getByText('Untimed.', { exact: false }));
    expect(onSeek).toHaveBeenLastCalledWith(3);
  });

  it('the merged paragraph reads as one text', () => {
    render(<TranscriptTab md={MD} onSeek={vi.fn()} />);
    expect(screen.getByTestId('transcript-turn-0').textContent).toContain(
      '00:00:03Let us start with the roadmap. Onboarding comes first. The vendor migration follows.',
    );
  });
});
