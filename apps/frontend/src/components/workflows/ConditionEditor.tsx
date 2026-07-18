import { useState } from 'react';
import type { ConditionNode, Contact, Tag } from '../../tauri';
import { TagChip } from '../tags/TagChip';
import { TagCombobox } from '../tags/TagCombobox';

/** Recursive builder for the workflow condition tree. Group nodes render an
 *  ALL/ANY toggle + children; leaves render their own small editor. Depth is
 *  visually capped at 6 (evaluation is unlimited; UI sanity only). */
const MAX_UI_DEPTH = 6;

const groupBox = (mode: 'all' | 'any'): React.CSSProperties => ({
  borderLeft: `3px solid ${mode === 'all' ? 'var(--indigo)' : 'var(--sunset)'}`,
  paddingLeft: 12,
  display: 'flex',
  flexDirection: 'column',
  gap: 8,
});

function AddConditionMenu({ onAdd }: { onAdd: (n: ConditionNode) => void }) {
  const [open, setOpen] = useState(false);
  const pick = (n: ConditionNode) => { onAdd(n); setOpen(false); };
  return (
    <span style={{ position: 'relative' }}>
      <button type="button" className="btn" onClick={() => setOpen((o) => !o)}>+ condition</button>
      {open && (
        <span style={{ display: 'inline-flex', gap: 6, marginLeft: 8 }}>
          <button type="button" className="btn" onClick={() => pick({ type: 'has_tag', tag_id: '' })}>Tag</button>
          <button type="button" className="btn" onClick={() => pick({ type: 'has_participant', contact_id: '' })}>Participant</button>
          <button type="button" className="btn" onClick={() => pick({ type: 'title_contains', needle: '' })}>Title contains</button>
        </span>
      )}
    </span>
  );
}

function LeafEditor({ node, onChange, tags, contacts }: {
  node: ConditionNode;
  onChange: (n: ConditionNode) => void;
  tags: Tag[];
  contacts: Contact[];
}) {
  if (node.type === 'has_tag') {
    const tag = tags.find((t) => t.id === node.tag_id);
    return (
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">tag is</span>
        {node.tag_id === '' ? (
          <TagCombobox excludeIds={[]} onPick={(t) => onChange({ type: 'has_tag', tag_id: t.id })} />
        ) : tag ? (
          <TagChip tag={tag} onRemove={() => onChange({ type: 'has_tag', tag_id: '' })} />
        ) : (
          <span className="meta" style={{ fontStyle: 'italic' }}>missing tag</span>
        )}
      </span>
    );
  }
  if (node.type === 'has_participant') {
    const known = node.contact_id === '' || contacts.some((c) => c.id === node.contact_id);
    return (
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">participant is</span>
        {known ? (
          <select
            value={node.contact_id}
            onChange={(e) => onChange({ type: 'has_participant', contact_id: e.target.value })}
          >
            <option value="">Choose a person…</option>
            {contacts.map((c) => (
              <option key={c.id} value={c.id}>{c.display_name}</option>
            ))}
          </select>
        ) : (
          <span className="meta" style={{ fontStyle: 'italic' }}>missing contact</span>
        )}
      </span>
    );
  }
  if (node.type === 'title_contains') {
    return (
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">title contains</span>
        <input
          type="text"
          value={node.needle}
          placeholder="e.g. design spec"
          onChange={(e) => onChange({ type: 'title_contains', needle: e.target.value })}
        />
      </span>
    );
  }
  return null;
}

export function ConditionEditor({ node, onChange, tags, contacts, depth = 0 }: {
  node: ConditionNode;
  onChange: (n: ConditionNode) => void;
  tags: Tag[];
  contacts: Contact[];
  depth?: number;
}) {
  if (node.type !== 'all' && node.type !== 'any') {
    return <LeafEditor node={node} onChange={onChange} tags={tags} contacts={contacts} />;
  }
  const children = node.children;
  const setChild = (i: number, n: ConditionNode) => {
    const next = children.slice();
    next[i] = n;
    onChange({ ...node, children: next });
  };
  const removeChild = (i: number) => {
    onChange({ ...node, children: children.filter((_, x) => x !== i) });
  };
  return (
    <div style={depth === 0 ? { display: 'flex', flexDirection: 'column', gap: 8 } : groupBox(node.type)}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <select
          aria-label="Group match mode"
          value={node.type}
          onChange={(e) => onChange({ type: e.target.value as 'all' | 'any', children })}
        >
          <option value="all">ALL of</option>
          <option value="any">ANY of</option>
        </select>
        {depth === 0 && children.length === 0 && (
          <span className="meta">No conditions = runs for every recording.</span>
        )}
      </div>
      {children.map((child, i) => (
        <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
          <div style={{ flex: 1 }}>
            <ConditionEditor node={child} onChange={(n) => setChild(i, n)} tags={tags} contacts={contacts} depth={depth + 1} />
          </div>
          <button type="button" className="btn" title="Remove" aria-label="Remove" onClick={() => removeChild(i)}>✕</button>
        </div>
      ))}
      <div style={{ display: 'flex', gap: 8 }}>
        <AddConditionMenu onAdd={(n) => onChange({ ...node, children: [...children, n] })} />
        {depth < MAX_UI_DEPTH && (
          <button
            type="button"
            className="btn"
            onClick={() => onChange({ ...node, children: [...children, { type: 'any', children: [] }] })}
          >
            + group
          </button>
        )}
      </div>
    </div>
  );
}
