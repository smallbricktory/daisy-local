// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import type { Tag } from '../../tauri';

vi.mock('../../tauri', () => ({
  tauri: {
    searchTags: vi.fn().mockResolvedValue([]),
    createTag: vi.fn(),
  },
}));

// Stable array reference — TagCombobox's search effect depends on the
// identity of useTags()'s return value; a fresh `[]` literal per call would
// destabilize that effect and re-render forever.
const mockNoTags = vi.hoisted(() => [] as Tag[]);
vi.mock('../../lib/useTags', () => ({
  useTags: () => mockNoTags,
  invalidateTags: vi.fn(),
}));

import { TagCombobox } from './TagCombobox';

function fakeTag(id: string, name: string): Tag {
  return {
    id,
    name,
    color_hex: '#F2A900',
    prompt_md: null,
    vocab_md: null,
    created_at_unix_seconds: 0,
    use_count: 0,
  };
}

describe('TagCombobox — Suggested section', () => {
  it('renders the Suggested section on top and accepts on click', () => {
    const onPick = vi.fn();
    render(
      <TagCombobox
        excludeIds={[]}
        onPick={onPick}
        suggested={[{ tag: fakeTag('t1', 'Brewhaven'), evidence: 'BH-Cloud ×2' }]}
        onDismissSuggested={vi.fn()}
      />,
    );
    fireEvent.focus(screen.getByPlaceholderText(/tag/i));
    expect(screen.getByText('Suggested')).toBeInTheDocument();
    fireEvent.click(screen.getByText('Brewhaven'));
    expect(onPick).toHaveBeenCalledWith(expect.objectContaining({ id: 't1' }));
  });

  it('dismiss control fires and does not pick', () => {
    const onPick = vi.fn();
    const onDismiss = vi.fn();
    render(
      <TagCombobox
        excludeIds={[]}
        onPick={onPick}
        suggested={[{ tag: fakeTag('t1', 'Brewhaven'), evidence: 'BH-Cloud ×2' }]}
        onDismissSuggested={onDismiss}
      />,
    );
    fireEvent.focus(screen.getByPlaceholderText(/tag/i));
    fireEvent.click(screen.getByLabelText('Dismiss suggestion Brewhaven'));
    expect(onDismiss).toHaveBeenCalledWith('t1');
    expect(onPick).not.toHaveBeenCalled();
  });

  it('does not render Suggested when the query is non-empty', async () => {
    const onPick = vi.fn();
    render(
      <TagCombobox
        excludeIds={[]}
        onPick={onPick}
        suggested={[{ tag: fakeTag('t1', 'Brewhaven'), evidence: 'BH-Cloud ×2' }]}
        onDismissSuggested={vi.fn()}
      />,
    );
    const input = screen.getByPlaceholderText(/tag/i);
    fireEvent.focus(input);
    fireEvent.change(input, { target: { value: 'zzz' } });
    expect(screen.queryByText('Suggested')).not.toBeInTheDocument();
    // Let the (mocked, resolved) searchTags effect settle before the test
    // ends, so its state update doesn't land outside act().
    await screen.findByText('+ create "zzz"');
  });

  it('renders with no suggested prop (backward compatible)', () => {
    const onPick = vi.fn();
    render(<TagCombobox excludeIds={[]} onPick={onPick} />);
    fireEvent.focus(screen.getByPlaceholderText(/tag/i));
    expect(screen.queryByText('Suggested')).not.toBeInTheDocument();
  });
});
