import { useEffect, useRef, useState } from 'react';
import { tauri, type Tag } from '../../tauri';
import { TagColorPicker } from './TagColorPicker';

export function TagCombobox({ excludeIds, onPick }: { excludeIds: string[]; onPick: (t: Tag) => void }) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [options, setOptions] = useState<Tag[]>([]);
  const [creating, setCreating] = useState(false);
  const [newColor, setNewColor] = useState('#FF6A00');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const q = query.trim();
    (q === '' ? tauri.listTags() : tauri.searchTags(q))
      .then((t) => { if (!cancelled) setOptions(t.filter((x) => !excludeIds.includes(x.id))); })
      .catch(() => { /* keep prior options */ });
    return () => { cancelled = true; };
  }, [open, query, excludeIds]);

  useEffect(() => {
    function onDoc(e: MouseEvent) { if (boxRef.current && !boxRef.current.contains(e.target as Node)) { setOpen(false); setCreating(false); } }
    document.addEventListener('mousedown', onDoc);
    return () => document.removeEventListener('mousedown', onDoc);
  }, []);

  const exactExists = options.some((o) => o.name.toLowerCase() === query.trim().toLowerCase());
  const canCreate = query.trim() !== '' && !exactExists;

  async function doCreate() {
    setBusy(true); setErr(null);
    try {
      const t = await tauri.createTag({ name: query.trim(), color_hex: newColor });
      onPick(t); setQuery(''); setOpen(false); setCreating(false);
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  }

  return (
    <div className="tag-combobox" ref={boxRef}>
      <input className="tag-combobox__input" placeholder="+ add tag" value={query}
        onFocus={() => setOpen(true)} onChange={(e) => { setQuery(e.target.value); setOpen(true); setCreating(false); }} />
      {open && (
        <div className="tag-combobox__menu">
          {options.map((t) => (
            <div key={t.id} className="tag-combobox__option" onClick={() => { onPick(t); setQuery(''); setOpen(false); }}>
              <span className="tag-chip__dot" style={{ background: t.color_hex }} /> {t.name}
              <span style={{ marginLeft: 'auto', color: 'var(--iron)', fontSize: 11 }}>{t.use_count}×</span>
            </div>
          ))}
          {canCreate && !creating && (
            <div className="tag-combobox__option tag-combobox__create" onClick={() => setCreating(true)}>+ create "{query.trim()}"</div>
          )}
          {canCreate && creating && (
            <div className="tag-combobox__option" style={{ flexDirection: 'column', alignItems: 'stretch', gap: 8 }} onClick={(e) => e.stopPropagation()}>
              <span style={{ color: 'var(--iron)', fontSize: 11 }}>Color for "{query.trim()}"</span>
              <TagColorPicker value={newColor} onChange={setNewColor} />
              {err && <span style={{ color: 'var(--danger)', fontSize: 11 }}>{err}</span>}
              <div style={{ display: 'flex', gap: 8 }}>
                <button className="btn btn--primary" disabled={busy} onClick={doCreate}>Create &amp; add</button>
                <button className="btn" disabled={busy} onClick={() => setCreating(false)}>Cancel</button>
              </div>
            </div>
          )}
          {options.length === 0 && !canCreate && <div className="tag-combobox__option" style={{ color: 'var(--iron)' }}>no tags yet — type a name to create one</div>}
        </div>
      )}
    </div>
  );
}
