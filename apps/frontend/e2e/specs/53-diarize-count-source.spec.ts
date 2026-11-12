// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The line under the speaker list naming where a re-diarize took its speaker
// count from, and the count field's placeholder.
import { $, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { nerdMode } from '../pages/settings.ts';
import { nav } from '../pages/shell.ts';
import { library } from '../pages/library.ts';
import { session, speakers } from '../pages/session.ts';

const DIARIZE = 'diarize_session';
const COUNT = 'diarize_count';
const SOURCE_LINE = '[data-testid="speakers-count-source"]';

async function openParticipants(): Promise<void> {
  await nav.go('library');
  await library.openRow('s-001');
  await session.open('participants');
}

describe('diarize count source', function () {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nerdMode.set(true);
  });

  afterEach(async () => {
    await restoreInvoke(DIARIZE);
    await restoreInvoke(COUNT);
  });

  it('the count field shows the invitee count as its placeholder', async () => {
    await openParticipants();
    await expect(speakers.recount()).toHaveAttribute('placeholder', '1');
  });

  it('an estimated count says so and points at the count field', async () => {
    await mockInvoke(DIARIZE, { speakers: 3, clustered: 3, count: 3, segments_labeled: 24, segments_seen: 24, count_source: 'auto' });
    await openParticipants();
    await expect($(SOURCE_LINE)).not.toBeExisting();
    await mockInvoke(COUNT, { source: 'auto', requested: null, found: 3 });
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).toHaveText(
      '3 speakers, estimated. Enter the number of speakers in the count field to correct it.',
    );
  });

  it('a count from the invitees names the calendar', async () => {
    await mockInvoke(DIARIZE, { speakers: 2, clustered: 2, count: 2, segments_labeled: 24, segments_seen: 24, count_source: 'attendees' });
    await openParticipants();
    await mockInvoke(COUNT, { source: 'attendees', requested: null, found: 2 });
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).toHaveText('2 speakers, from calendar invitees');
  });

  it('a typed count reads as entered', async () => {
    await mockInvoke(DIARIZE, { speakers: 2, clustered: 2, count: 2, segments_labeled: 24, segments_seen: 24, count_source: 'typed' });
    await openParticipants();
    await speakers.recount().setValue('2');
    await mockInvoke(COUNT, { source: 'typed', requested: 2, found: 2 });
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).toHaveText('2 speakers, as entered');
  });

  it('a typed count the run exceeded says how many voices split off', async () => {
    await mockInvoke(DIARIZE, { speakers: 3, clustered: 3, count: 3, segments_labeled: 24, segments_seen: 24, count_source: 'typed' });
    await openParticipants();
    await speakers.recount().setValue('2');
    await mockInvoke(COUNT, { source: 'typed', requested: 2, found: 3 });
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).toHaveText('You asked for 2 speakers; found 3 — 1 voice split off as a separate person.');
  });

  it('a count the last run recorded shows when the session is reopened', async () => {
    // A fresh page stands in for reopening the session.
    await bootWorld();
    await nerdMode.set(true);
    await mockInvoke(COUNT, { source: 'expected', requested: 4, found: 6 });
    await openParticipants();
    await expect($(SOURCE_LINE)).toHaveText('You expected 4 speakers; found 6 — 2 voices split off as separate people.');
  });

  it('a mic re-run beside kept remote speakers counts only the voices it grouped', async () => {
    await mockInvoke(DIARIZE, { speakers: 5, clustered: 3, count: 3, segments_labeled: 24, segments_seen: 24, count_source: 'typed' });
    await openParticipants();
    await speakers.recount().setValue('3');
    await mockInvoke(COUNT, { source: 'typed', requested: 3, found: 3 });
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).toHaveText('3 speakers, as entered');
  });

  it('a run that took one speaker from the recording setup adds no line', async () => {
    await mockInvoke(DIARIZE, { speakers: 1, clustered: 1, count: null, segments_labeled: 24, segments_seen: 24, count_source: null });
    await openParticipants();
    await speakers.rediarize().click();
    await expect($(SOURCE_LINE)).not.toBeExisting();
  });
});
