// Guards over tauri.conf.json choices that break silently when reverted.
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';

const conf = JSON.parse(readFileSync('../../crates/tauri-app/tauri.conf.json', 'utf8'));

describe('tauri.conf guards', () => {
  it('main window keeps dragDropEnabled: false — WebView2 native drag-drop swallows HTML5 DnD (nav reorder) when enabled', () => {
    expect(conf.app.windows[0].dragDropEnabled).toBe(false);
  });
});
