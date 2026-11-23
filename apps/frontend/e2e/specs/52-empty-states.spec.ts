// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// D5. The state a new install opens in: every list the app fills from the
// backend holding nothing. Each container names what belongs there and carries
// the step that puts it there, and that step works from the empty screen.
//
// Browser mode only — the empty world is the fixture's, and a native run holds
// whatever the profile under test holds.
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { analyzer, calendar, history, search } from '../pages/browse.ts';
import { library } from '../pages/library.ts';
import { nav } from '../pages/shell.ts';
import { workflows } from '../pages/workflows.ts';

/** A world whose every fillable list is empty. Sessions stay: a meeting's own
 *  chat and chapters are empty containers inside a populated library. */
/** A token no fixture meeting carries. */
const NO_SUCH_TOKEN = 'zzqq-no-such-meeting';

const NOTHING = {
  workflows: [], runs: [], queue: [], integrationRuns: [], events: [],
  chats: {}, chapters: {},
};

describe('empty states', () => {
  before(async function boot() {
    if (isNative()) this.skip();
    await bootWorld(NOTHING);
  });

  it('calendar: names what belongs there and syncs from the empty screen', async () => {
    await nav.go('calendar');
    await expect(calendar.empty()).toBeDisplayed();
    await expect(calendar.empty()).toHaveText(expect.stringContaining('No events in the next'));
    await expect(calendar.emptySync()).toBeEnabled();
    await calendar.emptySync().click();
    // Nothing arrives, so the screen keeps saying what belongs there.
    await expect(calendar.emptySync()).toBeEnabled();
    await expect(calendar.empty()).toBeDisplayed();
  });

  it('history: names both sources and leads to the one that fills it', async () => {
    await nav.go('history');
    await expect(history.empty()).toBeDisplayed();
    await expect(history.empty()).toHaveText(expect.stringContaining('Workflow runs and manual sends'));
    await history.emptyWorkflows().click();
    await expect(workflows.empty()).toBeDisplayed();
  });

  it('workflows: the empty list opens the editor it describes', async () => {
    await nav.go('tasks');
    await expect(workflows.empty()).toBeDisplayed();
    await workflows.emptyNew().click();
    await expect(workflows.name()).toBeDisplayed();
    await workflows.cancel().click();
    await expect(workflows.empty()).toBeDisplayed();
  });

  it('search: a query nothing matches says what search reads', async () => {
    await nav.go('search');
    await search.type(NO_SUCH_TOKEN);
    await expect(search.empty()).toBeDisplayed();
    await expect(search.empty()).toHaveText(expect.stringContaining('Try fewer words'));
  });

  it('library: a filter that matches nothing offers the way back', async () => {
    await nav.go('library');
    // Every tag in the fixture is on some meeting, so two of them together are
    // what no meeting carries — the filter is an AND.
    const tagIds = await library.filterTagIds();
    for (const id of tagIds.slice(0, 2)) await library.filter(id).click();
    await expect(library.emptyFiltered()).toBeDisplayed();
    await library.emptyClear().click();
    await expect(library.rows()).not.toBeElementsArrayOfSize(0);
  });

  describe('a profile that has never recorded', () => {
    before(async () => {
      await bootWorld({ ...NOTHING, sessions: [] });
    });

    it('library: names what lands here and opens the recorder', async () => {
      await nav.go('library');
      await expect(library.empty()).toBeDisplayed();
      await library.emptyRecord().click();
      await expect(nav.item('record')).toHaveElementClass('active');
    });

    it('analyzer: says what it needs and opens the recorder', async () => {
      await nav.go('analyzer');
      await expect(analyzer.empty()).toBeDisplayed();
      await analyzer.emptyRecord().click();
      await expect(nav.item('record')).toHaveElementClass('active');
    });
  });
});
