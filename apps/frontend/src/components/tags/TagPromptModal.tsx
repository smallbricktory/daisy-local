import { useState } from 'react';
import { tauri, type Tag } from '../../tauri';

export function TagPromptModal({ tag, onClose, onSaved }: { tag: Tag; onClose: () => void; onSaved: (t: Tag) => void }) {
  const [prompt, setPrompt] = useState(tag.prompt_md ?? '');
  const [vocab, setVocab] = useState(tag.vocab_md ?? '');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  async function save() {
    setBusy(true); setErr(null);
    try {
      const updated = await tauri.updateTag({
        id: tag.id,
        // "" (not null) clears the field: the backend's Option<Option<String>>
        // can't tell an explicit null from an absent field (both deserialize
        // to None = "untouched"). "" → the backend trims-to-None and clears.
        prompt_md: prompt.trim(),
        vocab_md: vocab.trim(),
      });
      onSaved(updated); onClose();
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  }
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h3 className="modal__title">Tag settings — {tag.name}</h3>

        <label style={{ fontSize: 12, fontWeight: 600 }}>Prompt</label>
        <p style={{ fontSize: 11, color: 'var(--iron)' }}>Instructions for the AI (summary + in-call chat). Wrapped in <code>&lt;tag_directives&gt;</code>; cannot override the engine's role or output schema.</p>
        <textarea className="summary-edit" style={{ minHeight: 140 }} value={prompt} onChange={(e) => setPrompt(e.target.value)}
          placeholder={'e.g.\nUse "Northwind Logistics" never "NWL".\nAction items must name the responsible owner.'} />

        <label style={{ fontSize: 12, fontWeight: 600, marginTop: 12 }}>Vocabulary</label>
        <p style={{ fontSize: 11, color: 'var(--iron)' }}>Names &amp; terms to spell correctly. Sent to the transcriber so it hears them right. Comma or line-separated.</p>
        <textarea className="summary-edit" style={{ minHeight: 100 }} value={vocab} onChange={(e) => setVocab(e.target.value)}
          placeholder={'Northwind Logistics, NWL, Priya Okonkwo, Project Aurora'} />

        {err && <p style={{ color: 'var(--danger)', fontSize: 12 }}>{err}</p>}
        <div className="modal__actions">
          <button className="btn btn--primary" disabled={busy} onClick={save}>Save</button>
          <button className="btn" disabled={busy} onClick={onClose}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
