// Publish the design-system gallery to a static directory — the local UI
// review is served by `python3 -m http.server`, which cannot resolve the
// gallery's /src/ stylesheet paths the way the vite dev server does.
//
// Copies the two stylesheets next to a rewritten copy of the page, so the
// published gallery renders the tokens as they are at publish time. Re-run it
// after any change to workspace.css, or the copy goes stale.
//
// Usage, from apps/frontend:
//   npm run publish:gallery -- ~/Sync/daisy/ui-review-2026-09-11
import { readFileSync, writeFileSync, mkdirSync, copyFileSync } from 'node:fs';
import { join } from 'node:path';

const OUT = process.argv[2];
if (!OUT) throw new Error('usage: publish-gallery.mjs <dir>');
mkdirSync(join(OUT, 'css'), { recursive: true });

for (const f of ['reset.css', 'workspace.css']) {
  copyFileSync(join('src/styles', f), join(OUT, 'css', f));
}

const html = readFileSync('test/gallery.html', 'utf8')
  .replace(/href="\/src\/styles\//g, 'href="css/');
writeFileSync(join(OUT, 'gallery.html'), html);

console.log(`published gallery.html + css/ to ${OUT}`);
