import { ReactNode } from 'react';

function renderInline(text: string): ReactNode[] {
  const out: ReactNode[] = [];
  const re = /(\*\*[^*]+\*\*|_[^_]+_)/g;
  let last = 0; let m: RegExpExecArray | null; let k = 0;
  while ((m = re.exec(text)) !== null) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const tok = m[0];
    if (tok.startsWith('**')) out.push(<strong key={k++}>{tok.slice(2, -2)}</strong>);
    else out.push(<em key={k++}>{tok.slice(1, -1)}</em>);
    last = m.index + tok.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function MarkdownView({ markdown, mono }: { markdown: string; mono?: boolean }) {
  const lines = markdown.replace(/\r\n/g, '\n').split('\n');
  const blocks: ReactNode[] = [];
  let i = 0; let key = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === '') { i++; continue; }
    if (line.startsWith('### ')) { blocks.push(<h3 key={key++}>{renderInline(line.slice(4))}</h3>); i++; continue; }
    if (line.startsWith('## ')) { blocks.push(<h2 key={key++}>{renderInline(line.slice(3))}</h2>); i++; continue; }
    if (line.startsWith('# ')) { blocks.push(<h1 key={key++}>{renderInline(line.slice(2))}</h1>); i++; continue; }
    if (line.startsWith('- ')) {
      const items: ReactNode[] = [];
      while (i < lines.length && lines[i].startsWith('- ')) { items.push(<li key={items.length}>{renderInline(lines[i].slice(2))}</li>); i++; }
      blocks.push(<ul key={key++}>{items}</ul>); continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i].trim() !== '' && !/^(#{1,3} |- )/.test(lines[i])) { para.push(lines[i]); i++; }
    blocks.push(<p key={key++}>{renderInline(para.join(' '))}</p>);
  }
  return <div className={mono ? 'markdown-view markdown-view--mono' : 'markdown-view'}>{blocks}</div>;
}
