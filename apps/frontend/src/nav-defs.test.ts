// Nav rail contract: default order, per-item icons, Help as an external
// entry inside the reorderable list.
import { describe, it, expect, vi } from 'vitest';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(), Channel: class {} }));
vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({ writeText: vi.fn() }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ save: vi.fn(), open: vi.fn() }));

import { NAV_DEFS } from './App';

describe('nav rail defaults', () => {
  it('orders Record, Library, Calendar, Search, Workflows, Analyzer, History, Help', () => {
    expect(NAV_DEFS.map((d) => d.key)).toEqual(
      ['record', 'library', 'calendar', 'search', 'tasks', 'analyzer', 'history', 'help'],
    );
  });

  it('every main item carries an icon (Record uses its dot instead)', () => {
    for (const d of NAV_DEFS) {
      if (d.recordStyle) continue;
      expect(d.icon, `${d.key} icon`).toBeTruthy();
    }
  });

  it('Help is external and routeless; every other item routes', () => {
    const help = NAV_DEFS.find((d) => d.key === 'help')!;
    expect(help.externalUrl).toMatch(/^https:\/\//);
    expect(help.route).toBeUndefined();
    for (const d of NAV_DEFS.filter((x) => x.key !== 'help')) {
      expect(d.route, `${d.key} route`).toBeTruthy();
    }
  });
});
