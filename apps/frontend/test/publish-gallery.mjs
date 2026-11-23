// Publishes the design-system gallery to a static directory: copies the two
// stylesheets next to a copy of the page with its stylesheet paths rewritten.
//
// Usage, from apps/frontend:
//   npm run publish:gallery -- <output-dir>
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
