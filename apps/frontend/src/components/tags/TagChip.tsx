import type { Tag } from '../../tauri';

function withAlpha(hex: string, alpha: number): string {
  let h = hex.replace('#', '');
  if (h.length === 3) h = h.split('').map((c) => c + c).join('');
  const r = parseInt(h.slice(0, 2), 16), g = parseInt(h.slice(2, 4), 16), b = parseInt(h.slice(4, 6), 16);
  if ([r, g, b].some((n) => Number.isNaN(n))) return `rgba(108,105,96,${alpha})`;
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export function TagChip({ tag, onEditPrompt, onRemove }: {
  tag: Pick<Tag, 'name' | 'color_hex'>;
  onEditPrompt?: () => void;
  onRemove?: () => void;
}) {
  return (
    <span className="tag-chip" style={{ background: withAlpha(tag.color_hex, 0.12), border: `1px solid ${tag.color_hex}`, color: tag.color_hex }}>
      <span className="tag-chip__dot" style={{ background: tag.color_hex }} />
      {tag.name}
      {onEditPrompt && <span className="tag-chip__pencil" title={`Edit ${tag.name} prompt`} onClick={(e) => { e.stopPropagation(); onEditPrompt(); }}>✎</span>}
      {onRemove && <span className="tag-chip__pencil" title={`Remove ${tag.name}`} onClick={(e) => { e.stopPropagation(); onRemove(); }}>×</span>}
    </span>
  );
}
