import { useEditor, EditorContent } from '@tiptap/react';
import StarterKit from '@tiptap/starter-kit';
import { Markdown } from 'tiptap-markdown';
import { useEffect } from 'react';

/**
 * Minimal markdown-backed rich-text editor, shared by the Summary editor and
 * the Notes tab. TipTap StarterKit gives headings/bold/italic/lists/quotes
 * with the usual markdown input shortcuts (`#`, `**`, `-` …); tiptap-markdown
 * round-trips the document as markdown (summary.md / notes.md stay plain
 * markdown).
 *
 * No toolbar — markdown shortcuts + a clean surface. `onChange` receives
 * serialized markdown on every edit.
 */
export function MarkdownEditor({
  value,
  onChange,
  autoFocus = false,
  minHeight = 220,
}: {
  value: string;
  onChange: (markdown: string) => void;
  autoFocus?: boolean;
  minHeight?: number;
}) {
  const editor = useEditor({
    extensions: [
      StarterKit,
      Markdown.configure({ html: false, linkify: true, breaks: true }),
    ],
    content: value,
    autofocus: autoFocus ? 'end' : false,
    onUpdate: ({ editor: e }) => {
      onChange((e.storage as unknown as { markdown: { getMarkdown: () => string } }).markdown.getMarkdown());
    },
  });

  // External value swaps (e.g. switching sessions) reset the document;
  // normal typing flows through onUpdate. The reset runs only when the
  // editor isn't focused.
  useEffect(() => {
    if (!editor || editor.isFocused) return;
    const current = (editor.storage as unknown as { markdown: { getMarkdown: () => string } }).markdown.getMarkdown();
    if (current !== value) editor.commands.setContent(value);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value, editor]);

  return (
    <div
      className="md-editor"
      style={{
        border: '1px solid var(--frost-deep)',
        borderRadius: 8,
        background: 'var(--cream-pure)',
        padding: '10px 14px',
        minHeight,
        cursor: 'text',
      }}
      onClick={() => editor?.chain().focus().run()}
    >
      <EditorContent editor={editor} />
    </div>
  );
}
