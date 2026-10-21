// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import type { Tag, VocabSuggestion, LastTimeSuggestion } from '../../tauri';
import { GhostSuggestionChips, LastTimeChip } from './SuggestedTags';

function fakeTag(id: string, name: string, color = '#F2A900'): Tag {
  return {
    id,
    name,
    color_hex: color,
    prompt_md: null,
    vocab_md: null,
    created_at_unix_seconds: 0,
    use_count: 0,
  };
}

describe('GhostSuggestionChips', () => {
  const tags = [fakeTag('t1', 'Brewhaven'), fakeTag('t2', 'Cloud Migration'), fakeTag('t3', 'Q3 Planning')];

  it('renders tag names in order and fires onAccept on click', () => {
    const suggestions: VocabSuggestion[] = [
      { tag_id: 't2', total_hits: 3, evidence: [{ term: 'migration', count: 2 }, { term: 'cloud', count: 1 }] },
      { tag_id: 't1', total_hits: 1, evidence: [{ term: 'brew', count: 1 }] },
    ];
    const onAccept = vi.fn();
    render(<GhostSuggestionChips suggestions={suggestions} tags={tags} onAccept={onAccept} />);

    const chips = screen.getAllByRole('button');
    expect(chips.map((c) => c.textContent)).toEqual(['Cloud Migration', 'Brewhaven']);

    fireEvent.click(screen.getByText('Cloud Migration'));
    expect(onAccept).toHaveBeenCalledWith('t2');
  });

  it('carries the evidence string in title and aria-label', () => {
    const suggestions: VocabSuggestion[] = [
      { tag_id: 't1', total_hits: 2, evidence: [{ term: 'brew', count: 2 }, { term: 'haven', count: 1 }] },
    ];
    render(<GhostSuggestionChips suggestions={suggestions} tags={tags} onAccept={vi.fn()} />);
    const chip = screen.getByLabelText('Suggested tag: Brewhaven');
    expect(chip).toHaveAttribute('title', 'brew ×2; haven ×1');
  });

  it('skips a suggestion whose tag id is not in the catalog', () => {
    const suggestions: VocabSuggestion[] = [
      { tag_id: 'ghost-id', total_hits: 1, evidence: [{ term: 'x', count: 1 }] },
      { tag_id: 't3', total_hits: 1, evidence: [{ term: 'plan', count: 1 }] },
    ];
    render(<GhostSuggestionChips suggestions={suggestions} tags={tags} onAccept={vi.fn()} />);
    expect(screen.getAllByRole('button').map((c) => c.textContent)).toEqual(['Q3 Planning']);
  });

  it('renders nothing when there are no suggestions', () => {
    const { container } = render(<GhostSuggestionChips suggestions={[]} tags={tags} onAccept={vi.fn()} />);
    expect(container.firstChild).toBeNull();
  });
});

describe('LastTimeChip', () => {
  const tags = [
    fakeTag('t1', 'Brewhaven'),
    fakeTag('t2', 'Cloud Migration'),
    fakeTag('t3', 'Q3 Planning'),
  ];

  it('collapsed preview shows the first two names and +N for the rest', () => {
    const suggestion: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2', 't3'] };
    render(<LastTimeChip suggestion={suggestion} tags={tags} onApply={vi.fn()} />);
    expect(screen.getByText(/Tags from last time: Brewhaven, Cloud Migration \+1/)).toBeInTheDocument();
  });

  it('collapsed preview omits +N when there are two or fewer tags', () => {
    const suggestion: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2'] };
    render(<LastTimeChip suggestion={suggestion} tags={tags} onApply={vi.fn()} />);
    expect(screen.getByText('✦ Tags from last time: Brewhaven, Cloud Migration')).toBeInTheDocument();
  });

  it('expand, deselect one, Apply fires onApply with the correct selected/deselected split', () => {
    const suggestion: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2'] };
    const onApply = vi.fn();
    render(<LastTimeChip suggestion={suggestion} tags={tags} onApply={onApply} />);

    fireEvent.click(screen.getByText(/Tags from last time/));
    // All on by default.
    const t2Chip = screen.getByText('Cloud Migration');
    fireEvent.click(t2Chip);
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));

    expect(onApply).toHaveBeenCalledWith(['t1'], ['t2']);
  });

  it('a changed suggestion prop keeps a newly-appeared id on by default', () => {
    // Simulates a pre-record title edit resolving to a different last-time
    // match while the chip stays mounted: t1 was deselected under the old
    // suggestion, then the suggestion changes to a set that no longer
    // includes t1 but newly includes t3. t3 must render on (not silently
    // absent from selection) and Apply must include it.
    const before: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2'] };
    const onApply = vi.fn();
    const { rerender } = render(<LastTimeChip suggestion={before} tags={tags} onApply={onApply} />);

    fireEvent.click(screen.getByText(/Tags from last time/));
    fireEvent.click(screen.getByText('Brewhaven')); // deselect t1

    const after: LastTimeSuggestion = { source_session_id: 's1', tag_ids: ['t2', 't3'] };
    rerender(<LastTimeChip suggestion={after} tags={tags} onApply={onApply} />);

    // t3 is new and must be on by default.
    const t3Chip = screen.getByText('Q3 Planning').closest('button');
    expect(t3Chip).toHaveClass('tag-chip--on');

    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    expect(onApply).toHaveBeenCalledWith(['t2', 't3'], []);
  });

  it('undo restoring a dismissed id renders it on, having never been locally deselected', () => {
    // Undo (sessionUndismissTagSuggestions) removes a dismissal server-side,
    // which can hand the chip a `suggestion` prop that regains an id it
    // didn't carry a moment ago — t2 briefly leaves (dismissed) then comes
    // back. t2 was never toggled off by the user in this component instance,
    // so it must render on, same as any other id present in `entries`.
    const withBoth: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2'] };
    const onApply = vi.fn();
    const { rerender } = render(<LastTimeChip suggestion={withBoth} tags={tags} onApply={onApply} />);
    fireEvent.click(screen.getByText(/Tags from last time/));

    const withoutT2: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1'] };
    rerender(<LastTimeChip suggestion={withoutT2} tags={tags} onApply={onApply} />);

    const restored: LastTimeSuggestion = { source_session_id: 's0', tag_ids: ['t1', 't2'] };
    rerender(<LastTimeChip suggestion={restored} tags={tags} onApply={onApply} />);

    const t2Chip = screen.getByText('Cloud Migration').closest('button');
    expect(t2Chip).toHaveClass('tag-chip--on');
  });
});
