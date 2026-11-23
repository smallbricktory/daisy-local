// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';
import type { ConditionNode, Contact, Tag } from '../../tauri';
import { TagChip } from '../tags/TagChip';
import { TagCombobox } from '../tags/TagCombobox';
import { Select } from '../Select';

/** Recursive builder for the workflow condition tree. Group nodes render an
 *  ALL/ANY toggle + children; leaves render their own small editor. Depth is
 *  visually capped at 6 (evaluation is unlimited; UI sanity only). */
const MAX_UI_DEPTH = 6;

const groupBox = (mode: 'all' | 'any'): React.CSSProperties => ({
  borderLeft: `3px solid ${mode === 'all' ? 'var(--gold)' : 'var(--gold-text)'}`,
  paddingLeft: 12,
  display: 'flex',
  flexDirection: 'column',
  gap: 8,
});

function AddConditionMenu({ onAdd, path }: {
  onAdd: (n: ConditionNode) => void;
  /** Index path of the group this menu adds to, for the test ids. */
  path: string;
}) {
  const [open, setOpen] = useState(false);
  const pick = (n: ConditionNode) => { onAdd(n); setOpen(false); };
  return (
    <span style={{ position: 'relative' }}>
      <button data-testid={`cond-add-${path}`} type="button" className="btn" onClick={() => setOpen((o) => !o)}>+ condition</button>
      {open && (
        <span style={{ display: 'inline-flex', gap: 6, marginLeft: 8 }}>
          <button data-testid={`cond-add-tag-${path}`} type="button" className="btn" onClick={() => pick({ type: 'has_tag', tag_id: '' })}>Tag</button>
          <button data-testid={`cond-add-participant-${path}`} type="button" className="btn" onClick={() => pick({ type: 'has_participant', contact_id: '' })}>Participant</button>
          <button data-testid={`cond-add-title-${path}`} type="button" className="btn" onClick={() => pick({ type: 'title_contains', needle: '' })}>Title contains</button>
        </span>
      )}
    </span>
  );
}

function LeafEditor({ node, onChange, tags, contacts, path }: {
  node: ConditionNode;
  onChange: (n: ConditionNode) => void;
  tags: Tag[];
  contacts: Contact[];
  /** This node's index path in the tree, for the test ids. */
  path: string;
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
          <Select
            testId={`cond-select-${path}`}
            label="Participant"
            value={node.contact_id}
            onChange={(v) => onChange({ type: 'has_participant', contact_id: v })}
            options={[
              { value: '', label: 'Choose a person…' },
              ...contacts.map((c) => ({ value: c.id, label: c.display_name })),
            ]}
          />
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
          data-testid={`cond-title-${path}`}
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

export function ConditionEditor({ node, onChange, tags, contacts, depth = 0, path = '0' }: {
  node: ConditionNode;
  onChange: (n: ConditionNode) => void;
  tags: Tag[];
  contacts: Contact[];
  depth?: number;
  /** This node's index path in the tree: the root is `0`, its i-th child
   *  `0-i`. Carries into the test ids so a nested editor is addressable. */
  path?: string;
}) {
  if (node.type !== 'all' && node.type !== 'any') {
    return <LeafEditor node={node} onChange={onChange} tags={tags} contacts={contacts} path={path} />;
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
        <Select
          testId={`cond-mode-${path}`}
          label="Group match mode"
          value={node.type}
          onChange={(v) => onChange({ type: v as 'all' | 'any', children })}
          options={[
            { value: 'all', label: 'ALL of' },
            { value: 'any', label: 'ANY of' },
          ]}
        />
        {depth === 0 && children.length === 0 && (
          <span className="meta">No conditions = runs for every recording.</span>
        )}
      </div>
      {children.map((child, i) => (
        <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
          <div style={{ flex: 1 }}>
            <ConditionEditor node={child} onChange={(n) => setChild(i, n)} tags={tags} contacts={contacts} depth={depth + 1} path={`${path}-${i}`} />
          </div>
          <button data-testid={`cond-remove-${path}-${i}`} type="button" className="btn" title="Remove" aria-label="Remove" onClick={() => removeChild(i)}>✕</button>
        </div>
      ))}
      <div style={{ display: 'flex', gap: 8 }}>
        <AddConditionMenu path={path} onAdd={(n) => onChange({ ...node, children: [...children, n] })} />
        {depth < MAX_UI_DEPTH && (
          <button
            data-testid={`cond-add-group-${path}`}
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
