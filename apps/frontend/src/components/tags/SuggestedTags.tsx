// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Summary-pane suggestion surfaces: dashed "ghost" chips for vocab hits and
// the collapsible "tags from last time" chip. Both are quiet — no floating
// layers, no blur — flat panels styled from the same tokens as the rest of
// the chip family (`.tag-chip--ghost` / `--on` / `--off`).

import { useState } from 'react';
import type { LastTimeSuggestion, SessionSuggestions, Tag, VocabSuggestion } from '../../tauri';

function evidenceText(evidence: VocabSuggestion['evidence']): string {
  return evidence.map((e) => `${e.term} ×${e.count}`).join('; ');
}

export function GhostSuggestionChips({
  suggestions,
  tags,
  onAccept,
}: {
  suggestions: VocabSuggestion[];
  tags: Tag[];
  onAccept: (tagId: string) => void;
}) {
  const entries = suggestions
    .map((s) => ({ suggestion: s, tag: tags.find((t) => t.id === s.tag_id) }))
    .filter((e): e is { suggestion: VocabSuggestion; tag: Tag } => e.tag != null);

  if (entries.length === 0) return null;

  return (
    <>
      {entries.map(({ suggestion, tag }) => (
        <button
          key={tag.id}
          data-testid={`tag-ghost-${tag.id}`}
          type="button"
          className="tag-chip tag-chip--button tag-chip--ghost"
          style={{ '--tag-color': tag.color_hex } as React.CSSProperties}
          title={evidenceText(suggestion.evidence)}
          aria-label={`Suggested tag: ${tag.name}`}
          onClick={() => onAccept(tag.id)}
        >
          <span className="tag-chip__dot" />
          {tag.name}
        </button>
      ))}
    </>
  );
}

export function LastTimeChip({
  suggestion,
  tags,
  onApply,
}: {
  suggestion: LastTimeSuggestion;
  tags: Tag[];
  onApply: (selectedIds: string[], deselectedIds: string[]) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const entries = suggestion.tag_ids
    .map((id) => tags.find((t) => t.id === id))
    .filter((t): t is Tag => t != null);
  // Tracks ids the user has turned OFF, not ids turned on. A chip is on iff
  // its id is absent from this set — so when `suggestion` changes while
  // mounted (a pre-record title edit resolves to a different match, or Undo
  // restores a dismissed id), the newly-present id was never added here and
  // renders on by default.
  const [deselected, setDeselected] = useState<Set<string>>(() => new Set());

  if (entries.length === 0) return null;

  const names = entries.map((t) => t.name);
  const previewNames = names.slice(0, 2).join(', ');
  const extra = names.length > 2 ? ` +${names.length - 2}` : '';

  function toggle(id: string) {
    setDeselected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id); else next.add(id);
      return next;
    });
  }

  function apply() {
    const selectedIds = entries.filter((t) => !deselected.has(t.id)).map((t) => t.id);
    const deselectedIds = entries.filter((t) => deselected.has(t.id)).map((t) => t.id);
    onApply(selectedIds, deselectedIds);
  }

  return (
    <div className="last-time-chip">
      <button
        data-testid="last-time-chip-toggle"
        type="button"
        className="tag-chip tag-chip--button tag-chip--ghost"
        onClick={() => setExpanded((e) => !e)}
      >
        ✦ Tags from last time: {previewNames}{extra}
      </button>
      {expanded && (
        <div className="last-time-chip__panel">
          {entries.map((t) => {
            const on = !deselected.has(t.id);
            return (
              <button
                key={t.id}
                data-testid={`last-time-tag-${t.id}`}
                type="button"
                className={`tag-chip tag-chip--button ${on ? 'tag-chip--on' : 'tag-chip--off'}`}
                style={{ '--tag-color': t.color_hex } as React.CSSProperties}
                onClick={() => toggle(t.id)}
              >
                <span className="tag-chip__dot" />
                {t.name}
              </button>
            );
          })}
          <button data-testid="last-time-apply" type="button" className="btn btn--primary last-time-chip__apply" onClick={apply}>
            Apply
          </button>
        </div>
      )}
    </div>
  );
}

/** Feeds `TagCombobox`'s Suggested section: last-time tag ids merged with
 *  vocab-hit tags, applied ids dropped, deduped (vocab evidence wins for
 *  display), mapped to `{ tag, evidence }` via the catalog. */
export function buildSuggested(
  suggestions: SessionSuggestions | null,
  tags: Tag[],
  appliedTagIds: string[],
): Array<{ tag: Tag; evidence: string }> {
  if (!suggestions) return [];
  const applied = new Set(appliedTagIds);
  const byId = new Map<string, { tag: Tag; evidence: string }>();

  for (const id of suggestions.last_time?.tag_ids ?? []) {
    if (applied.has(id)) continue;
    const tag = tags.find((t) => t.id === id);
    if (!tag) continue;
    byId.set(id, { tag, evidence: 'from last time' });
  }

  for (const v of suggestions.vocab) {
    if (applied.has(v.tag_id)) continue;
    const tag = tags.find((t) => t.id === v.tag_id);
    if (!tag) continue;
    byId.set(v.tag_id, { tag, evidence: evidenceText(v.evidence) });
  }

  return Array.from(byId.values());
}
