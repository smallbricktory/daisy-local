// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the Library route: the session list, its tag filter, the
// new-meeting modal and the rough-audio warning.
import { browser, expect } from '@wdio/globals';
import { expectRowWithText, idsOf, tid, tids, typeInto, within } from './_shared.ts';
import { toast } from './shell.ts';
import {
  callsOf, mockInvoke, mockInvokeReject, mockInvokeWith, restoreInvoke,
} from '../fixtures/invoke.ts';
import { FIXTURE_WAV } from '../env.ts';

/** The file-picker command the new-meeting modal calls. */
const DIALOG_OPEN = 'plugin:dialog|open';

/** The import command behind the modal's primary button. */
const IMPORT_AUDIO = 'import_audio_meeting';

/** The lifecycle store's single toast id; the finalize cascade drives it. */
const SESSION_STATUS = 'session-status';

export const library = {
  newMeeting: () => tid('library-new-meeting'),
  filter: (tagId: string) => tid(`library-filter-${tagId}`),
  filterClear: () => tid('library-filter-clear'),
  /** What the list says while it holds nothing: never recorded, and filtered
   *  down to nothing. Each carries the step that fills it again. */
  empty: () => tid('library-empty'),
  /** Tag ids the filter row offers, in display order. */
  async filterTagIds(): Promise<string[]> {
    return (await idsOf('library-filter-')).filter((id) => id !== 'clear');
  },
  emptyRecord: () => tid('library-empty-record'),
  emptyFiltered: () => tid('library-empty-filtered'),
  emptyClear: () => tid('library-empty-clear'),
  row: (id: string) => tid(`library-row-${id}`),
  // `rows()` below scans for the `library-row-` prefix, so an id nested inside
  // a row must not start with it — it would be counted as a row.

  /** The ghost-chip holder on an untagged row; absent when the matcher found
   *  the session no vocabulary hit. */
  ghosts: (sessionId: string) => tid(`library-ghosts-${sessionId}`),
  /** One ghost chip, scoped to its row: the summary pane renders `tag-ghost-`
   *  ids for the open session at the same time. */
  ghost: (sessionId: string, tagId: string) => within(tid(`library-ghosts-${sessionId}`), `tag-ghost-${tagId}`),
  /** The chip a row wears when its audio did not come from this computer:
   *  SYNC (synced from a device) or IMPORTED (from an audio file). Absent
   *  on a meeting recorded or typed here. */
  kind: (id: string) => tid(`library-row-${id}`).$('[data-testid="session-kind"]'),
  rows: () => tids('library-row-'),
  async rowCount(): Promise<number> {
    return (await this.rows()).length;
  },
  async expectRowCount(n: number) {
    await expect(this.rows()).toBeElementsArrayOfSize(n);
  },
  /** Session ids in display order. */
  async rowIds(): Promise<string[]> {
    return idsOf('library-row-');
  },
  /** Waits for a row whose text carries `title`. Rows created inside one
   *  second share a `created_at`, so position is not assertable — presence is. */
  async expectRow(title: string) {
    await expectRowWithText('library-row-', title, `no library row titled "${title}"`);
  },
  async rowIdByTitle(title: string): Promise<string> {
    for (const el of await this.rows()) {
      if (!(await el.getText()).includes(title)) continue;
      const id = await el.getAttribute('data-testid');
      if (id !== null) return id.replace('library-row-', '');
    }
    throw new Error(`no library row titled "${title}"`);
  },
  async firstRowId(): Promise<string> {
    const ids = await this.rowIds();
    if (ids.length === 0) throw new Error('the library list is empty');
    return ids[0];
  },
  /** Clicks a row and waits for its detail pane. */
  async openRow(id: string) {
    await this.row(id).click();
    await expect(tid('session-title')).toBeDisplayed();
  },
  /** Narrows the list to sessions carrying `tagId`. */
  async applyFilter(tagId: string, expectRows: number) {
    await this.filter(tagId).click();
    await this.expectRowCount(expectRows);
  },
  async clearFilter(expectRows: number) {
    await this.filterClear().click();
    await expect(this.filterClear()).not.toBeExisting();
    await this.expectRowCount(expectRows);
  },
};

/** The best-effort warning shown when imported audio decodes but reads rough. */
export const importWarn = {
  scrim: () => tid('import-warn-scrim'),
  modal: () => tid('import-warn'),
  cancel: () => tid('import-warn-cancel'),
  proceed: () => tid('import-warn-proceed'),
};

export const newMeeting = {
  scrim: () => tid('new-meeting-scrim'),
  modal: () => tid('new-meeting'),
  title: () => tid('new-meeting-title'),
  chooseAudio: () => tid('new-meeting-choose-audio'),
  clearAudio: () => tid('new-meeting-clear-audio'),
  speakers: () => tid('new-meeting-speakers'),
  notes: () => tid('new-meeting-notes'),
  tag: (id: string) => tid(`new-meeting-tag-${id}`),
  cancel: () => tid('new-meeting-cancel'),
  submit: () => tid('new-meeting-submit'),
  async open() {
    await library.newMeeting().click();
    await expect(this.modal()).toBeDisplayed();
  },
  async cancelOut() {
    await this.cancel().click();
    await expect(this.modal()).not.toBeExisting();
  },
  async createNote(title: string, notes: string) {
    await this.open();
    await typeInto('new-meeting-title', title);
    await typeInto('new-meeting-notes', notes);
    await this.submit().click();
    await expect(this.modal()).not.toBeExisting();
  },
  /** Points the file picker at the fixture clip and imports it. The dialog
   *  command is overridden in the page, so both modes take the same path; in
   *  native the real import and its cascade run. */
  async importFixture(title: string) {
    await mockInvoke(DIALOG_OPEN, FIXTURE_WAV);
    try {
      await this.open();
      await typeInto('new-meeting-title', title);
      await this.chooseAudio().click();
      // The picked path reveals the speaker-count field and flips the primary
      // button to the import wording.
      await expect(this.speakers()).toBeDisplayed();
      await expect(this.submit()).toHaveText(expect.stringContaining('Import'));
      await this.submit().click();
      await browser.waitUntil(
        async () => (await importWarn.modal().isExisting()) || !(await this.modal().isExisting()),
        { timeoutMsg: 'the new-meeting modal neither closed nor raised the rough-audio warning' },
      );
      if (await importWarn.modal().isExisting()) await importWarn.proceed().click();
      await expect(this.modal()).not.toBeExisting();
    } finally {
      await restoreInvoke(DIALOG_OPEN);
    }
  },
  /** How many times the modal has opened the file picker this run. */
  async pickerCalls() {
    return callsOf(DIALOG_OPEN);
  },
  /** Picks a file whose path the dialog's own request args decide, and reads
   *  the chosen name back off the modal. The reply runs in the page with no
   *  closure, so the path is spelled out inside it. */
  async pickAudioFromArgs() {
    await mockInvokeWith(DIALOG_OPEN, (args) => (
      JSON.stringify(args ?? {}).includes('wav')
        ? '/Users/alex/Recordings/board-review.wav'
        : null
    ));
    try {
      await this.chooseAudio().click();
      await expect(this.modal()).toHaveText(expect.stringContaining('board-review.wav'));
      await expect(this.submit()).toHaveText(expect.stringContaining('Import'));
    } finally {
      await restoreInvoke(DIALOG_OPEN);
    }
  },
  /** Cancelling the picker resolves `null`, leaving the meeting note-only. */
  async cancelPicker() {
    await mockInvoke(DIALOG_OPEN, null);
    try {
      await this.chooseAudio().click();
      await expect(this.speakers()).not.toBeExisting();
      await expect(this.submit()).toHaveText(expect.stringContaining('Create'));
    } finally {
      await restoreInvoke(DIALOG_OPEN);
    }
  },
  /** Drives an import the backend refuses, and reads the reason off the modal.
   *  `friendly`/`code` mirror the serialized AppError the real command throws. */
  async importRejected(title: string, friendly: string, code: string) {
    await mockInvoke(DIALOG_OPEN, FIXTURE_WAV);
    await mockInvokeReject(IMPORT_AUDIO, { kind: 'import', code, message: friendly, friendly });
    try {
      await this.open();
      await typeInto('new-meeting-title', title);
      await this.chooseAudio().click();
      await expect(this.speakers()).toBeDisplayed();
      await this.submit().click();
      await expect(this.modal()).toHaveText(expect.stringContaining(`${friendly} (${code})`));
      await this.cancelOut();
    } finally {
      await restoreInvoke(IMPORT_AUDIO);
      await restoreInvoke(DIALOG_OPEN);
    }
  },
  /** Waits for the finalize cascade to leave its working stages. The
   *  session-status toast titles the session "… is ready" at the speaker-label
   *  gate and "… transcribed ✓" / "… ready ✓" when the tail completes; both
   *  mean transcription finished. */
  async waitForTranscribed(title: string, timeout = 170_000) {
    const status = () => toast.byId(SESSION_STATUS);
    await browser.waitUntil(async () => {
      if (!(await status().isExisting())) return false;
      const text = await status().getText();
      return text.includes(title) && /is ready|ready ✓|transcribed ✓/.test(text);
    }, {
      timeout,
      timeoutMsg: `finalize cascade for "${title}" did not report a transcript within ${timeout} ms`,
    });
  },
};
