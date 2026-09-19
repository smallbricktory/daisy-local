// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the four browsing routes: Search (keyword + Q&A), Calendar,
// History and Analyzer.
import { browser, expect } from '@wdio/globals';
import {
  expectPicked, idsOf, optionValueByText, optionValues, selectOption, tid, tids, typeIntoEl,
} from './_shared.ts';

export const search = {
  query: () => tid('search-query'),
  /** The block a query with no hit paints. */
  empty: () => tid('search-empty'),
  ask: () => tid('search-ask'),
  cancel: () => tid('search-cancel'),
  filters: () => tid('search-filters'),
  from: () => tid('search-from'),
  to: () => tid('search-to'),
  clearDates: () => tid('search-clear-dates'),
  result: (id: string) => tid(`search-result-${id}`),
  results: () => tids('search-result-'),
  /** One snippet inside a result row; clicking it opens that detail tab. */
  match: (sessionId: string, i: number) => tid(`search-match-${sessionId}-${i}`),
  tag: (id: string) => tid(`search-tag-${id}`),
  /** A selected tag renders as the shared removable chip. */
  tagChipRemove: (id: string) => tid(`tag-chip-remove-${id}`),
  person: (id: string) => tid(`search-person-${id}`),
  /** The finished Q&A answer panel. */
  answer: () => tid('search-answer'),
  citation: (i: number) => tid(`search-citation-${i}`),

  /** How many hits the last pass left on screen. */
  async resultCount(): Promise<number> {
    return (await this.results()).length;
  },

  /** Session ids of the listed hits, in display order. */
  async resultIds(): Promise<string[]> {
    return idsOf('search-result-');
  },

  /** Types a query; the box keeps it. A keyword pass runs on a debounce, a
   *  query ending in `?` waits for Ask. */
  async type(q: string) {
    await typeIntoEl(this.query(), q);
    await expect(this.query()).toHaveValue(q);
  },

  /** Types a keyword query and waits for the debounced pass to land at least
   *  one hit. */
  async runQuery(q: string) {
    await this.type(q);
    await browser.waitUntil(async () => (await this.resultCount()) > 0, {
      timeoutMsg: `the query "${q}" never returned a hit`,
    });
  },

  /** Empties the query box one keystroke at a time; the box reports empty. */
  async clearQuery() {
    await this.query().click();
    const len = (await this.query().getValue()).length;
    if (len > 0) await browser.keys(new Array(len).fill('Backspace'));
    await expect(this.query()).toHaveValue('');
  },

  /** Opens the filter panel; the date boxes are its content. */
  async openFilters() {
    await this.filters().click();
    await expect(this.from()).toBeDisplayed();
  },

  /** Ids of the tags the filter panel offers as unselected chips. A selected
   *  one renders as a tag chip instead and leaves this list. */
  async tagIds(): Promise<string[]> {
    return idsOf('search-tag-');
  },

  /** Ids of the people the filter panel offers; selecting one keeps its id. */
  async personIds(): Promise<string[]> {
    return idsOf('search-person-');
  },

  /** Sets both date bounds, then clears them; the boxes empty. */
  async setDates(from: string, to: string) {
    await this.from().setValue(from);
    await this.to().setValue(to);
    await expect(this.clearDates()).toBeDisplayed();
  },

  async clearDateFilter() {
    await this.clearDates().click();
    await expect(this.from()).toHaveValue('');
    await expect(this.to()).toHaveValue('');
  },
};

export const calendar = {
  window: () => tid('calendar-window'),
  sync: () => tid('calendar-sync'),
  /** The block an empty window paints, and the sync it offers. */
  empty: () => tid('calendar-empty'),
  emptySync: () => tid('calendar-empty-sync'),
  /** The line the last Sync reported: what was scanned and loaded. */
  note: () => tid('calendar-note'),
  event: (id: string) => tid(`calendar-event-${id}`),
  events: () => tids('calendar-event-'),
  dismiss: (id: string) => tid(`calendar-dismiss-${id}`),

  /** Picks the look-ahead window in days; the select keeps it and the list
   *  reloads. */
  async chooseWindow(days: number) {
    await selectOption('calendar-window', String(days));
    await expectPicked(this.window(), String(days));
  },

  /** Runs one sync and waits for the button to re-arm. Syncing reaches every
   *  subscribed ICS host, so a spec calls this only where `refresh_calendars`
   *  is mocked. */
  async syncNow() {
    await expect(this.sync()).toBeEnabled();
    await this.sync().click();
    await expect(this.sync()).toBeEnabled();
  },

  /** Hides one event; the dismissal is stored on its subscription. */
  async dismissEvent(id: string) {
    await this.dismiss(id).click();
    await expect(this.event(id)).not.toBeExisting();
  },
};

export const history = {
  refresh: () => tid('history-refresh'),
  /** The block an empty list paints, and the route it sends the reader to. */
  empty: () => tid('history-empty'),
  emptyWorkflows: () => tid('history-empty-workflows'),
  more: () => tid('history-more'),
  expand: (runId: string) => tid(`history-expand-${runId}`),
  /** The meeting link on a workflow-run row. */
  runOpen: (sessionId: string) => tid(`history-run-open-${sessionId}`),
  /** The meeting link on a manual-send row. */
  sessionOpen: (sessionId: string) => tid(`history-session-open-${sessionId}`),

  /** Refetches both stores and waits for the reloaded list to hold `runId`. */
  async refreshNow(runId: string) {
    await this.refresh().click();
    await expect(this.expand(runId)).toBeDisplayed();
  },

  /** Refetches both stores and waits for `runId` to leave the list. */
  async refreshWithout(runId: string) {
    await this.refresh().click();
    await expect(this.expand(runId)).not.toBeExisting();
  },

  /** Opens one run's step list. The row marks itself with the caret. */
  async expandRun(runId: string) {
    await this.expand(runId).click();
    await expect(this.expand(runId)).toHaveText(expect.stringContaining('▾'));
  },

  async collapseRun(runId: string) {
    await this.expand(runId).click();
    await expect(this.expand(runId)).toHaveText(expect.stringContaining('▸'));
  },

  /** Text of one expanded run's step rows: the cell holding them is the one
   *  under the row carrying the expand button. */
  async stepsText(runId: string): Promise<string> {
    return browser.execute((testid: string) => {
      const button = document.querySelector(`[data-testid="${testid}"]`);
      const row = button?.closest('tr');
      return row?.nextElementSibling?.textContent ?? '';
    }, `history-expand-${runId}`);
  },
};

export const analyzer = {
  session: () => tid('analyzer-session'),
  /** The block a profile with no transcript paints, and the step it offers. */
  empty: () => tid('analyzer-empty'),
  emptyRecord: () => tid('analyzer-empty-record'),
  openLibrary: () => tid('analyzer-open-library'),
  prompt: () => tid('analyzer-prompt'),
  directive: () => tid('analyzer-directive'),
  run: () => tid('analyzer-run'),
  saveAs: () => tid('analyzer-save-as'),
  save: () => tid('analyzer-save'),
  newName: () => tid('analyzer-new-name'),
  newSave: () => tid('analyzer-new-save'),
  newCancel: () => tid('analyzer-new-cancel'),
  /** The rendered result markdown. */
  output: () => tid('analyzer-output'),

  /** Whether any meeting is eligible. The picker is rendered only for
   *  sessions that carry a transcript. */
  async hasSessions(): Promise<boolean> {
    return this.session().isExisting();
  },

  /** Id of the topic the picker offers under this name, or null. */
  async promptIdByName(name: string): Promise<string | null> {
    return optionValueByText('analyzer-prompt', name);
  },

  /** Session id of the picker's first option, or null when it offers none. */
  async firstSessionId(): Promise<string | null> {
    const values = await optionValues('analyzer-session');
    return values[0] ?? null;
  },

  /** Picks a meeting; the picker keeps it. */
  async chooseSession(id: string) {
    await selectOption('analyzer-session', id);
    await expectPicked(this.session(), id);
  },

  /** Picks a topic. `adhoc` is the free-text one, which empties the box and
   *  leaves no unsaved-edit guard armed. */
  async choosePrompt(id: string) {
    await selectOption('analyzer-prompt', id);
    await expectPicked(this.prompt(), id);
  },

  async setDirective(text: string) {
    await typeIntoEl(this.directive(), text);
    await expect(this.directive()).toHaveValue(text);
  },

  /** Types the name the save-as row will store; the box keeps it. */
  async typeNewName(name: string) {
    await typeIntoEl(this.newName(), name);
    await expect(this.newName()).toHaveValue(name);
  },

  /** Copies the shown directive into a new stored prompt, which the page then
   *  selects — so Save changes, which only a stored non-builtin prompt has,
   *  appears. Saving writes the vault, so a spec pairs this with a teardown. */
  async saveAsNew(name: string) {
    await this.saveAs().click();
    await this.typeNewName(name);
    await this.newSave().click();
    await expect(this.save()).toBeDisplayed();
  },

  /** Leaves the save-as row closed whether or not it is open. */
  async ensureSaveAsClosed() {
    if (await this.newCancel().isExisting()) await this.newCancel().click();
    await expect(this.newName()).not.toBeExisting();
  },
};
