// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { analyzer, calendar, history, search } from '../pages/browse.ts';
import { aiStatus, calendars, prompts, settings } from '../pages/settings.ts';
import { nav } from '../pages/shell.ts';

const QA_ASK = 'qa_ask_stream';
const RUN_ANALYSIS = 'run_analysis';

/** A token no fixture and no recording carries, so the index answers with
 *  nothing. */
const NO_SUCH_TOKEN = 'zzqqwx';

/** Seeded browser-world ids. */
const SEEDED_SESSION = 's-001';
const SEEDED_SESSION_WORD = 'roadmap';
const SEEDED_EVENT = 'ev-1';
const SEEDED_RUN = 'r-1';
const SEEDED_RUN_STEP = 'My weekly digest';

/** The question form: any query ending in `?` flips Search into Q&A. */
const A_QUESTION = 'what was decided about the roadmap?';
/** The answer the browser world serves. */
const A_SEEDED_ANSWER = 'The roadmap is due Friday.';

/** Native has no calendar until one is subscribed, and the nav entry appears
 *  only then. `.test` is a reserved TLD (RFC 6761) and resolves nowhere; the
 *  ICS fetch happens on Sync, which this spec mocks in both modes. */
const A_CALENDAR = 'E2E browse calendar';
const AN_ICS_URL = 'https://calendar.example.test/e2e-browse.ics';

/** One workflow run, in the record shape `workflow_history_read` returns. The
 *  native profile has run none, so its History row comes from this. */
const A_RUN_ID = 'r-e2e';
const A_RUN_STEP = 'E2E digest step';
const A_RUN = {
  run_id: A_RUN_ID,
  at_unix_seconds: 1_756_000_000,
  workflow_id: 'w-e2e',
  workflow_name: 'E2E after finalize',
  session_id: 's-e2e',
  session_title: 'E2E seeded meeting',
  trigger: 'finalized',
  status: 'ok',
  steps: [{ label: A_RUN_STEP, status: 'ok', duration_ms: 1840 }],
};

/** The prompt the analyzer's Save as… writes. */
const A_SAVED_PROMPT = 'E2E analyzer topic';
const A_DIRECTIVE = 'List the risks raised in this meeting.';

describe('search, calendar, history, analyzer', () => {
  before(async () => {
    await bootWorld();
    // Native runs as a machine whose AI provider is ready, so Q&A and the
    // analyzer run; the AI commands themselves are mocked where they fire.
    if (isNative()) await aiStatus.pin({ state: 'Configured', provider: 'local', hint: null });
  });

  after(async () => {
    if (isNative()) await aiStatus.release();
  });

  it('search: an empty query lists every session and a miss narrows to none', async () => {
    await nav.go('search');
    // The empty box runs the same command with no tokens, which keeps every
    // session — so the list is the profile's, whatever this run has recorded.
    await browser.waitUntil(async () => (await search.resultCount()) > 0, {
      timeoutMsg: 'the empty query listed no session',
    });
    const all = await search.resultCount();

    await search.type(NO_SUCH_TOKEN);
    await browser.waitUntil(async () => (await search.resultCount()) === 0, {
      timeoutMsg: `"${NO_SUCH_TOKEN}" still matched a session`,
    });
    // Q&A mode is off, so neither the Ask nor the Cancel button is rendered.
    await expect(search.ask()).not.toBeExisting();
    await expect(search.cancel()).not.toBeExisting();

    await search.clearQuery();
    await browser.waitUntil(async () => (await search.resultCount()) === all, {
      timeoutMsg: 'clearing the query did not bring the sessions back',
    });
  });

  it('search: a keyword hit shows where it matched (browser)', async function () {
    if (isNative()) {
      // The native index holds whatever the earlier spec files recorded; no
      // title is fixed by this spec, so there is no keyword to assert on.
      console.log('[e2e] native titles are not fixed by this spec; keyword-match case skipped');
      return this.skip();
    }
    await search.runQuery(SEEDED_SESSION_WORD);
    await expect(search.result(SEEDED_SESSION)).toBeDisplayed();
    await expect(search.match(SEEDED_SESSION, 0)).toHaveText(expect.stringContaining(SEEDED_SESSION_WORD));
    await search.clearQuery();
  });

  it('search: the filter panel offers tags, people and a date range', async () => {
    await nav.go('search');
    await search.openFilters();
    const tagIds = await search.tagIds();
    const personIds = await search.personIds();
    if (tagIds.length > 0) {
      // Selecting one replaces the chip with a removable one, so it leaves
      // the offered list.
      await search.tag(tagIds[0]).click();
      await expect(search.tag(tagIds[0])).not.toBeExisting();
      await search.tagChipRemove(tagIds[0]).click();
      await expect(search.tag(tagIds[0])).toBeDisplayed();
    }
    if (personIds.length > 0) {
      await search.person(personIds[0]).click();
      await expect(search.person(personIds[0])).toHaveText(expect.stringContaining('✕'));
      await search.person(personIds[0]).click();
    }
    await search.setDates('2026-01-01', '2026-12-31');
    await search.clearDateFilter();
    await search.filters().click();
    await expect(search.from()).not.toBeExisting();
  });

  it('search: a question answers from the index', async () => {
    await nav.go('search');
    if (isNative()) await mockInvoke(QA_ASK, { query: A_QUESTION, answer: A_SEEDED_ANSWER, citations: [], indexed_sessions: 1, total_chunks: 1 });
    try {
      await search.type(A_QUESTION);
      await expect(search.ask()).toBeEnabled();
      await search.ask().click();
      await expect(search.answer()).toHaveText(expect.stringContaining(A_SEEDED_ANSWER));
    } finally {
      if (isNative()) await restoreInvoke(QA_ASK);
      await search.clearQuery();
    }
  });

  it('calendar: the window narrows, Sync reports its result, and an event is dismissed', async function () {
    // Syncing reaches every subscribed ICS host. The command is mocked in both
    // modes so the run makes no outbound request. The app also refreshes in
    // the background once a subscription exists, through the same command, so
    // the click is proven by the result it renders and never by a call count.
    await mockInvoke('refresh_calendars', { subscriptions_scanned: 1, events_loaded: 0, errors: [] });
    try {
      if (isNative()) {
        // The nav entry is rendered only while a subscription exists.
        await settings.open('calendars');
        await calendars.addSubscription(A_CALENDAR, AN_ICS_URL);
      }
      await nav.go('calendar');
      await calendar.chooseWindow(3);
      await calendar.syncNow();
      await expect(calendar.note()).toHaveText('Synced 1 calendar · 0 events.');

      if (isNative()) {
        // Nothing was fetched, so the list stays empty.
        await expect(calendar.events()).toBeElementsArrayOfSize(0);
        return;
      }
      await expect(calendar.event(SEEDED_EVENT)).toBeDisplayed();
      await calendar.dismissEvent(SEEDED_EVENT);
    } finally {
      // Idempotent: it clears a subscription the case failed part-way through
      // as readily as one it finished with.
      if (isNative()) await calendars.ensureAbsent(A_CALENDAR);
      await restoreInvoke('refresh_calendars');
    }
  });

  it('history: Refresh reloads and a run row expands to its steps', async () => {
    await nav.go('history');
    const runId = isNative() ? A_RUN_ID : SEEDED_RUN;
    const step = isNative() ? A_RUN_STEP : SEEDED_RUN_STEP;
    try {
      if (isNative()) {
        // The native profile has run no workflow, so the real read answers
        // without this run; the row under test comes from the mocked one.
        await history.refreshWithout(A_RUN_ID);
        await mockInvoke('workflow_history_read', [A_RUN]);
      }
      await history.refreshNow(runId);
      await expect(history.runOpen(isNative() ? 's-e2e' : SEEDED_SESSION)).toBeDisplayed();
      await history.expandRun(runId);
      await expect(await history.stepsText(runId)).toContain(step);
      await history.collapseRun(runId);
      // Both stores fit in one page, so there is nothing more to load.
      await expect(history.more()).not.toBeExisting();
    } finally {
      if (isNative()) {
        await restoreInvoke('workflow_history_read');
        await history.refreshWithout(A_RUN_ID);
      }
    }
  });

  it('analyzer: a run renders markdown', async function () {
    await nav.go('analyzer');
    // Ad Hoc text runs as typed and arms no unsaved-edit guard.
    await analyzer.choosePrompt('adhoc');
    await analyzer.setDirective(A_DIRECTIVE);
    if (!(await analyzer.hasSessions())) {
      console.log('[e2e] this profile has no meeting with a transcript; the Analyzer run is not offered');
      await expect(analyzer.run()).toBeDisabled();
      return this.skip();
    }
    const sessionId = await analyzer.firstSessionId();
    if (sessionId === null) throw new Error('the meeting picker offers no option');
    await analyzer.chooseSession(sessionId);
    await expect(analyzer.openLibrary()).toBeDisplayed();
    if (isNative()) {
      await mockInvoke(RUN_ANALYSIS, {
        prompt_id: 'adhoc', prompt_name: 'Ad Hoc', markdown: '## Headlines\n- Roadmap agreed', generated_at_unix_seconds: 0,
      });
    }
    try {
      await analyzer.run().click();
      await expect(analyzer.output()).toHaveText(expect.stringContaining('Headlines'));
    } finally {
      if (isNative()) await restoreInvoke(RUN_ANALYSIS);
    }
  });

  it('analyzer: Save as… stores the typed directive as a new prompt', async () => {
    await nav.go('analyzer');
    await analyzer.choosePrompt('adhoc');
    await analyzer.setDirective(A_DIRECTIVE);
    try {
      await expect(analyzer.saveAs()).toBeEnabled();
      await analyzer.saveAsNew(A_SAVED_PROMPT);
      // The page selects what it stored, so the editor now offers Save changes.
      await expect(analyzer.directive()).toHaveValue(A_DIRECTIVE);
      await expect(analyzer.save()).toBeDisabled();
    } finally {
      await analyzer.ensureSaveAsClosed();
      await prompts.ensureDeleted(A_SAVED_PROMPT);
    }
    await expect(await prompts.idByName(A_SAVED_PROMPT)).toBe(null);
  });

  it('analyzer: Save as… can be abandoned', async () => {
    await nav.go('analyzer');
    await analyzer.choosePrompt('adhoc');
    await analyzer.setDirective(A_DIRECTIVE);
    try {
      await analyzer.saveAs().click();
      await analyzer.typeNewName('E2E discarded topic');
      await analyzer.newCancel().click();
      await expect(analyzer.newName()).not.toBeExisting();
    } finally {
      await analyzer.ensureSaveAsClosed();
    }
    await expect(await analyzer.promptIdByName('E2E discarded topic')).toBe(null);
  });
});
