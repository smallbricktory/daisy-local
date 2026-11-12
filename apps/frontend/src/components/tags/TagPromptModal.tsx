// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';
import { tauri, type Tag } from '../../tauri';
import { invalidateTags } from '../../lib/useTags';

export function TagPromptModal({ tag, onClose, onSaved }: { tag: Tag; onClose: () => void; onSaved?: (t: Tag) => void }) {
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
      void invalidateTags();
      onSaved?.(updated); onClose();
    } catch (e) { setErr(String(e)); } finally { setBusy(false); }
  }
  return (
    <div data-testid="tag-prompt-scrim" className="modal-backdrop" onClick={onClose}>
      <div data-testid="tag-prompt" className="modal" onClick={(e) => e.stopPropagation()}>
        <h3 className="modal__title">Tag settings — {tag.name}</h3>

        <label className="field-label--strong">Prompt</label>
        <p className="meta" style={{ fontSize: 'var(--t-meta)' }}>Instructions for the AI (summary + in-call chat). Wrapped in <code>&lt;tag_directives&gt;</code>; cannot override the engine's role or output schema.</p>
        <textarea data-testid="tag-prompt-directive" className="summary-edit" style={{ minHeight: 140 }} value={prompt} onChange={(e) => setPrompt(e.target.value)}
          placeholder={'e.g.\nUse "Northwind Logistics" never "NWL".\nAction items must name the responsible owner.'} />

        <label className="field-label--strong" style={{ marginTop: 12 }}>Vocabulary</label>
        <p className="meta" style={{ fontSize: 'var(--t-meta)' }}>Names &amp; terms to spell correctly. Sent to the transcriber so it hears them right. Comma or line-separated.</p>
        <textarea data-testid="tag-prompt-vocab" className="summary-edit" style={{ minHeight: 100 }} value={vocab} onChange={(e) => setVocab(e.target.value)}
          placeholder={'Northwind Logistics, NWL, Priya Okonkwo, Project Aurora'} />

        {err && <p className="meta--danger" style={{ fontSize: 'var(--t-small)' }}>{err}</p>}
        <div className="modal__actions">
          <button data-testid="tag-prompt-save" className="btn btn--primary" disabled={busy} onClick={save}>Save</button>
          <button data-testid="tag-prompt-cancel" className="btn" disabled={busy} onClick={onClose}>Cancel</button>
        </div>
      </div>
    </div>
  );
}
