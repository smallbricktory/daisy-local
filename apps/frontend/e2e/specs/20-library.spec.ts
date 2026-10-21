// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { expect } from '@wdio/globals';
import { isNative, sessionDir } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { nav } from '../pages/shell.ts';
import { library, newMeeting } from '../pages/library.ts';

/** Rows the world seeds; the native sandbox starts on an empty profile. */
const SEEDED = isNative() ? 0 : 5;
const NOTE_TITLE = 'Kickoff notes';
const IMPORT_TITLE = 'Imported clip';

describe('library', () => {
  before(async () => {
    await bootWorld();
    await nav.go('library');
  });

  it('lists the sessions the profile holds', async () => {
    await library.expectRowCount(SEEDED);
  });

  it('creates a note-only meeting and lists it', async () => {
    await newMeeting.createNote(NOTE_TITLE, 'Agenda: scope, dates, owners');
    await library.expectRowCount(SEEDED + 1);
    await library.expectRow(NOTE_TITLE);
  });

  it('new-meeting cancel closes without creating', async () => {
    const before = await library.rowCount();
    await newMeeting.open();
    await newMeeting.cancelOut();
    await library.expectRowCount(before);
  });

  it('imports the fixture wav and transcribes it', async function () {
    // Native runs the real cascade on the 30 s clip.
    this.timeout(300_000);
    await newMeeting.importFixture(IMPORT_TITLE);
    await library.expectRowCount(SEEDED + 2);
    await library.expectRow(IMPORT_TITLE);
    if (!isNative()) return;
    await newMeeting.waitForTranscribed(IMPORT_TITLE);
    // The toast reports the cascade's stage; the transcript on disk is the
    // artifact it claims to have produced.
    const id = await library.rowIdByTitle(IMPORT_TITLE);
    expect(existsSync(join(sessionDir(id), 'transcript.json'))).toBe(true);
  });

  it('sorts sessions newest first', async function () {
    // Native starts empty, so both its rows were created in this run and no
    // older session exists to sort against.
    if (isNative()) this.skip();
    // The two rows this spec created precede every seeded session; s-001 is
    // the newest of those.
    expect((await library.rowIds()).indexOf('s-001')).toBe(2);
  });

  it('the import opened the file picker exactly once', async () => {
    const calls = await newMeeting.pickerCalls();
    expect(calls).toHaveLength(1);
  });

  it('a picked file shows its name and arms the import', async () => {
    await newMeeting.open();
    await newMeeting.pickAudio();
    await newMeeting.cancelOut();
  });

  it('cancelling the picker leaves the meeting note-only', async () => {
    await newMeeting.open();
    await newMeeting.cancelPicker();
    await newMeeting.cancelOut();
  });

  it('a refused import keeps the modal open and names the reason', async () => {
    const before = await library.rowCount();
    await newMeeting.importRejected('Doomed import', 'That audio file could not be read.', 'E_IMPORT');
    await library.expectRowCount(before);
  });

  it('the header has no search entry; the rail opens Search', async () => {
    await expect($('[data-testid="library-search-toggle"]')).not.toBeExisting();
    await expect($('[data-testid="library-search"]')).not.toBeExisting();
    await nav.go('search');
    await expect(nav.item('search')).toHaveElementClass('active');
    await nav.go('library');
  });

  it('a tag filter narrows the list and clear restores it', async function () {
    // The filter bar only renders for tags attached to a session; the native
    // sandbox has none.
    if (isNative()) this.skip();
    await library.applyFilter('t-1', 1);
    await library.clearFilter(SEEDED + 2);
  });

  it('a row says when its audio came from somewhere else', async function () {
    // The browser world seeds one synced meeting; the native sandbox
    // has whatever this run imported.
    const ids = await library.rowIds();
    const chipped: string[] = [];
    for (const id of ids) {
      if (await library.kind(id).isExisting()) chipped.push(id);
    }
    if (chipped.length === 0) this.skip();
    for (const id of chipped) {
      await expect(library.kind(id)).toHaveText(expect.stringMatching(/^(SYNC|IMPORTED)$/));
    }
    // The note this suite typed was made here, so it wears no chip.
    const noteId = await library.rowIdByTitle(NOTE_TITLE);
    await expect(library.kind(noteId)).not.toBeExisting();
  });

  it('opens a row into the detail pane', async () => {
    await library.openRow(await library.firstRowId());
  });
});
