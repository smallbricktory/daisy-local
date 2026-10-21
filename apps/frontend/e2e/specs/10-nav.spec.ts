// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { bootWorld } from '../fixtures/install.ts';
import { nav, type NavKey } from '../pages/shell.ts';

/** Calendar is excluded: its nav entry exists only while a calendar
 *  subscription does. Help is excluded: it opens an external URL. */
const ROUTED: Exclude<NavKey, 'mini' | 'calendar'>[] = [
  'record', 'library', 'search', 'tasks', 'analyzer', 'history', 'settings',
];

describe('navigation', () => {
  before(async () => {
    await bootWorld();
  });

  for (const k of ROUTED) {
    it(`opens ${k}`, async () => {
      await nav.go(k);
    });
  }

  it('opens calendar when a subscription exists', async () => {
    if (!(await nav.has('calendar'))) {
      console.log('[e2e] no calendar subscription; nav-calendar skipped');
      return;
    }
    await nav.go('calendar');
  });

  it('the record item carries the record style', async () => {
    await nav.expectRecordStyle();
  });

  it('ends on the library', async () => {
    await nav.go('library');
  });
});
