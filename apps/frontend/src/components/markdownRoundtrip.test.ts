import { describe, it, expect } from 'vitest';
import { Editor } from '@tiptap/react';
import StarterKit from '@tiptap/starter-kit';
import { Markdown } from 'tiptap-markdown';

// Mirror MarkdownEditor's exact config, then round-trip markdown through it.
// This is the fidelity check: what the user saves after a no-op edit must not
// drift in ways that break downstream parsing (summaryBody's TL;DR strip) or
// surprise them ("my formatting changed").
function roundtrip(md: string): string {
  const editor = new Editor({
    extensions: [StarterKit, Markdown.configure({ html: false, linkify: true, breaks: true })],
    content: md,
  });
  const out = (editor.storage as unknown as { markdown: { getMarkdown: () => string } }).markdown.getMarkdown();
  editor.destroy();
  return out;
}

describe('tiptap-markdown round-trip fidelity', () => {
  it('preserves the TL;DR marker summaryBody depends on', () => {
    const md = '# Weekly sync\n\n**TL;DR.** We shipped it.\n\n## Notes\n\nDetails here.';
    const out = roundtrip(md);
    // summaryBody regex: /\*\*TL;DR\.?\*\*[^\n]*\n+/i
    expect(out).toMatch(/\*\*TL;DR\.?\*\*/i);
  });

  it('keeps headings and bold', () => {
    const out = roundtrip('# Title\n\n**bold** text');
    expect(out).toContain('# Title');
    expect(out).toContain('**bold**');
  });

  it('reports list-marker behavior (informational)', () => {
    const out = roundtrip('- one\n- two\n- three');
    // Just surface what it emits — '-' vs '*' drift is cosmetic but real.
    expect(out).toMatch(/^[-*] one/m);
  });

  it('round-trips a realistic summary stably on second pass', () => {
    const md = '# Standup\n\n**TL;DR.** Three updates.\n\n## Decisions\n\n- Ship Friday\n- Defer X\n\n## Action items\n\n1. Alice: docs\n2. Bob: tests';
    const once = roundtrip(md);
    const twice = roundtrip(once);
    // Idempotent after the first normalization (what matters: edit→save→edit→save doesn't keep drifting).
    expect(twice).toBe(once);
  });
});
