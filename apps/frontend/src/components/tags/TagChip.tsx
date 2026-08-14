import type { Tag } from '../../tauri';

export function TagChip({ tag, onEditPrompt, onRemove }: {
  tag: Pick<Tag, 'name' | 'color_hex'>;
  onEditPrompt?: () => void;
  onRemove?: () => void;
}) {
  // The chip publishes the tag's colour as a custom property and lets CSS
  // mix the fill, ring and label from it — so the dark theme can quiet the
  // wash without every caller recomputing alphas.
  return (
    <span className="tag-chip" style={{ '--tag-color': tag.color_hex } as React.CSSProperties}>
      <span className="tag-chip__dot" />
      {tag.name}
      {onEditPrompt && <span className="tag-chip__pencil" title={`Edit ${tag.name} prompt`} onClick={(e) => { e.stopPropagation(); onEditPrompt(); }}>✎</span>}
      {onRemove && <span className="tag-chip__pencil" title={`Remove ${tag.name}`} onClick={(e) => { e.stopPropagation(); onRemove(); }}>×</span>}
    </span>
  );
}
