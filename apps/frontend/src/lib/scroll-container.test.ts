// Code that resets scroll must target the element the stylesheet actually
// scrolls. `.main` is `overflow: hidden` and `.main__body` carries
// `overflow-y: auto`, so scrolling `.main` is a silent no-op — Settings
// sections would open at whatever position the previous view left behind.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const CSS = readFileSync('src/styles/workspace.css', 'utf8');
const SETTINGS = readFileSync('src/routes/Settings.tsx', 'utf8');

/** The declaration block for a selector, or null. */
function block(selector: string): string | null {
  const m = CSS.match(
    new RegExp(`(^|\\n)\\s*${selector.replace('.', '\\.')}\\s*\\{([^}]*)\\}`),
  );
  return m ? m[2] : null;
}

describe('settings scroll reset targets the real scroller', () => {
  it('.main does not scroll', () => {
    const b = block('.main');
    expect(b).not.toBeNull();
    expect(b).toMatch(/overflow:\s*hidden/);
  });

  it('.main__body is the scrolling container', () => {
    const b = block('.main__body');
    expect(b).not.toBeNull();
    expect(b).toMatch(/overflow-y:\s*auto/);
  });

  it('Settings resets .main__body, not .main', () => {
    expect(SETTINGS).toMatch(/querySelector\('\.main__body'\)[\s\S]{0,160}scrollTo/);
    expect(SETTINGS).not.toMatch(/querySelector\('\.main'\)[\s\S]{0,160}scrollTo/);
  });
});
