// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitLibraryChanged } from '../fixtures/events.ts';
import { nav, toast } from '../pages/shell.ts';
import { library } from '../pages/library.ts';
import { record } from '../pages/record.ts';
import { lastTimeChip, tagging } from '../pages/session.ts';
import { typeIntoEl } from '../pages/_shared.ts';

/** `s-002` draws a vocabulary hit on `vendor`; `s-004` draws the tags `s-001`
 *  carried. Both start untagged, which is what puts the surfaces on screen —
 *  `s-001` already carries one and draws nothing. */
const VOCAB_SESSION = 's-002';
const LAST_TIME_SESSION = 's-004';
const TAGGED_SESSION = 's-001';
const VENDOR = 't-2';
const ROADMAP = 't-1';
const STANDUP = 't-3';

/** A title the world's pre-record matcher answers to. */
const PRERECORD_TITLE = 'Standup — Thursday';

describe('tag suggestions', () => {
  before(async function () {
    // The native sandbox starts on an empty profile: no earlier session to
    // draw last-time tags from, no transcript to match vocabulary against.
    if (isNative()) this.skip();
    await bootWorld();
    await nav.go('library');
  });

  it('an untagged row carries a ghost chip for the vocabulary it hit', async () => {
    await expect(library.ghost(VOCAB_SESSION, VENDOR)).toHaveText(expect.stringContaining('vendor'));
    await expect(library.ghosts(TAGGED_SESSION)).not.toBeExisting();
  });

  it('dismissing a suggestion drops it, and Undo puts it back', async () => {
    await library.openRow(LAST_TIME_SESSION);
    await tagging.openPicker();
    await expect(tagging.suggested(ROADMAP)).toBeDisplayed();
    await expect(tagging.suggested(STANDUP)).toBeDisplayed();

    // The undo toast expires after six seconds, so the round trip between the
    // dismiss and the Undo click stays short.
    await tagging.dismissSuggested(STANDUP).click();
    const undoToast = `tag-suggest-undo-${LAST_TIME_SESSION}-${STANDUP}`;
    await expect(toast.byId(undoToast)).toBeDisplayed();
    await emitLibraryChanged(LAST_TIME_SESSION);
    await tagging.openPicker();
    await expect(tagging.suggested(STANDUP)).not.toBeExisting();
    await expect(tagging.suggested(ROADMAP)).toBeDisplayed();

    await toast.action(undoToast, 'undo').click();
    await emitLibraryChanged(LAST_TIME_SESSION);
    await tagging.openPicker();
    await expect(tagging.suggested(STANDUP)).toBeDisplayed();
  });

  it('the last-time chip attaches what stays selected and drops what does not', async () => {
    await lastTimeChip.expand();
    await lastTimeChip.tag(STANDUP).click();
    await lastTimeChip.apply().click();
    await emitLibraryChanged(LAST_TIME_SESSION);

    await expect(tagging.chipRemove(ROADMAP)).toBeExisting();
    await expect(tagging.chipRemove(STANDUP)).not.toBeExisting();
    // The chip gives way to the applied tags, and the tag turned off is
    // dismissed rather than left waiting in the picker.
    await expect(lastTimeChip.toggle()).not.toBeExisting();
    await tagging.openPicker();
    await expect(tagging.suggested(STANDUP)).not.toBeExisting();
  });

  it('accepting a ghost from the library tags the session', async () => {
    await library.ghost(VOCAB_SESSION, VENDOR).click();
    await emitLibraryChanged(VOCAB_SESSION);
    await expect(library.ghosts(VOCAB_SESSION)).not.toBeExisting();

    await library.openRow(VOCAB_SESSION);
    await expect(tagging.chipRemove(VENDOR)).toBeExisting();
  });

  it('a pre-record title the matcher recognises offers last time’s tags', async () => {
    await nav.go('record');
    await expect(lastTimeChip.toggle()).not.toBeExisting();
    await typeIntoEl(record.title(), PRERECORD_TITLE);
    await expect(lastTimeChip.toggle()).toBeDisplayed();
    await lastTimeChip.expand();
    await expect(lastTimeChip.tag(STANDUP)).toBeDisplayed();
  });
});
