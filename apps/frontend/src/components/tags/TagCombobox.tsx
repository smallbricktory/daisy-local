// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { tauri, type Tag } from '../../tauri';
import { invalidateTags, useTags } from '../../lib/useTags';
import { DEFAULT_TAG_COLOR, TagColorPicker } from './TagColorPicker';
import { useDismiss } from '../../lib/useDismiss';
import { useAnchoredPop } from '../../lib/useAnchoredPop';

const POP_MAX_HEIGHT = 240;

export function TagCombobox({
  excludeIds,
  onPick,
  suggested = [],
  onDismissSuggested,
}: {
  excludeIds: string[];
  onPick: (t: Tag) => void;
  /** Ordered, pre-filtered by the caller (already-applied tags excluded). */
  suggested?: Array<{ tag: Tag; evidence: string }>;
  onDismissSuggested?: (tagId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [options, setOptions] = useState<Tag[]>([]);
  const [creating, setCreating] = useState(false);
  const [newColor, setNewColor] = useState(DEFAULT_TAG_COLOR);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const popRef = useRef<HTMLDivElement>(null);
  const popStyle = useAnchoredPop(open, boxRef, POP_MAX_HEIGHT);

  const allTags = useTags();
  /** Ids the Suggested section is already showing; the list below drops them while that section is up. */
  const suggestedKey = suggested.map((s) => s.tag.id).join(',');
  useEffect(() => {
    if (!open) return;
    const q = query.trim();
    const alreadyShown = suggestedKey === '' ? [] : suggestedKey.split(',');
    const hidden = q === '' ? [...excludeIds, ...alreadyShown] : excludeIds;
    if (q === '') {
      setOptions(allTags.filter((x) => !hidden.includes(x.id)));
      return;
    }
    let cancelled = false;
    tauri.searchTags(q)
      .then((t) => { if (!cancelled) setOptions(t.filter((x) => !hidden.includes(x.id))); })
      .catch(() => { /* keep prior options */ });
    return () => { cancelled = true; };
  }, [open, query, excludeIds, suggestedKey, allTags]);

  useDismiss(open, [boxRef, popRef], () => { setOpen(false); setCreating(false); });

  const exactExists = options.some((o) => o.name.toLowerCase() === query.trim().toLowerCase());
  const canCreate = query.trim() !== '' && !exactExists;

  async function doCreate() {
    setBusy(true); setErr(null);
    try {
      const t = await tauri.createTag({ name: query.trim(), color_hex: newColor });
      // Cache first, then pick — consumers that look the tag up by id (e.g.
      // the recorder's chip row) must already see it.
      await invalidateTags();
      onPick(t); setQuery(''); setOpen(false); setCreating(false);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  }

  return (
    <div className="tag-combobox" ref={boxRef}>
      <input data-testid="tag-combobox-input" className="tag-combobox__input" placeholder="+ add tag" value={query}
        onFocus={() => setOpen(true)} onChange={(e) => { setQuery(e.target.value); setOpen(true); setCreating(false); }} />
      {open && popStyle && createPortal(
        <div ref={popRef} className="menu tag-combobox__pop" style={popStyle}>
          {suggested.length > 0 && query.trim() === '' && (
            <>
              <div className="tag-combobox__section">Suggested</div>
              {suggested.map(({ tag, evidence }) => (
                <div key={tag.id} className="menu__item tag-combobox__suggested" title={evidence}>
                  <button data-testid={`tag-combobox-suggested-${tag.id}`} type="button" className="tag-combobox__suggested-pick"
                    style={{ '--tag-color': tag.color_hex } as React.CSSProperties}
                    onClick={() => { onPick(tag); setQuery(''); setOpen(false); }}>
                    <span className="tag-chip__dot" />
                    <span>{tag.name}</span>
                    <span className="tag-combobox__evidence">{evidence}</span>
                  </button>
                  <button data-testid={`tag-combobox-dismiss-${tag.id}`} type="button" className="tag-combobox__dismiss"
                    aria-label={`Dismiss suggestion ${tag.name}`}
                    onClick={() => onDismissSuggested?.(tag.id)}>×</button>
                </div>
              ))}
              <div className="menu__sep" role="presentation" />
              <div className="tag-combobox__section">All tags</div>
            </>
          )}
          {options.map((t) => (
            <div key={t.id} data-testid={`tag-combobox-option-${t.id}`} className="menu__item" onClick={() => { onPick(t); setQuery(''); setOpen(false); }}>
              <span className="tag-chip__dot" style={{ '--tag-color': t.color_hex } as React.CSSProperties} /> {t.name}
              <span className="meta" style={{ marginLeft: 'auto', fontSize: 'var(--t-meta)' }}>{t.use_count}×</span>
            </div>
          ))}
          {canCreate && !creating && (
            <div data-testid="tag-combobox-open-create" className="menu__item tag-combobox__create" onClick={() => setCreating(true)}>+ create "{query.trim()}"</div>
          )}
          {canCreate && creating && (
            <div data-testid="tag-combobox-create-form" className="menu__item tag-combobox__create-form" onClick={(e) => e.stopPropagation()}>
              <span className="tag-combobox__create-label">Color for "{query.trim()}"</span>
              <TagColorPicker value={newColor} onChange={setNewColor} />
              {err && <span className="tag-combobox__create-error">{err}</span>}
              <div className="tag-combobox__create-actions">
                <button data-testid="tag-combobox-create" type="button" className="btn btn--primary" disabled={busy} onClick={doCreate}>Create &amp; add</button>
                <button data-testid="tag-combobox-cancel" type="button" className="btn" disabled={busy} onClick={() => setCreating(false)}>Cancel</button>
              </div>
            </div>
          )}
          {options.length === 0 && !canCreate && <div className="menu__item menu__item--note">no tags yet — type a name to create one</div>}
        </div>,
        document.body,
      )}
    </div>
  );
}
