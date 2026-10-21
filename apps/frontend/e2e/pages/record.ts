// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the recorder: the pre-recording picker, the live notebook,
// and the two auxiliary webviews (mini card, meeting reminder).
import { $$, browser, expect } from '@wdio/globals';
import { Key } from 'webdriverio';
import { clickRowMenuItem, tid, typeInto } from './_shared.ts';
import { confirmDialog, nav } from './shell.ts';

export const record = {
  // Pre-recording picker.
  title: () => tid('record-title'),
  mic: () => tid('record-mic'),
  /** The level bar's fill; zero-width on a silent input. */
  micLevelFill: () => tid('mic-level-fill'),
  micLevelUnavailable: () => tid('mic-level-unavailable'),
  singleSpeaker: () => tid('record-single-speaker'),
  start: () => tid('record-start'),
  /** The tag box on whichever of the two screens is mounted. `ActiveSession`
   *  returns early from the picker branch, so the picker's combobox and the
   *  notebook's are never in the document together and the shared
   *  `tag-combobox-input` id stays unambiguous. */
  tagInput: () => tid('tag-combobox-input'),

  // Live notebook.
  liveTitle: () => tid('live-title'),
  liveMic: () => tid('live-mic'),
  mute: () => tid('live-mute'),
  notes: () => tid('live-notes'),
  /** Caption lines in the live transcript, and the speaker label above each. */
  lines: () => $$('[data-testid="live-line"]'),
  who: () => $$('[data-testid="live-line-who"]'),
  noteClose: () => tid('live-note-close'),
  pause: () => tid('live-pause'),
  resume: () => tid('live-resume'),
  finish: () => tid('live-finish'),
  overflow: () => tid('live-overflow'),
  discard: () => tid('live-discard'),

  /** Titles and starts a recording, landing on the live notebook. */
  async begin(title: string) {
    await typeInto('record-title', title);
    await this.start().click();
    await expect(this.pause()).toBeDisplayed();
  },

  /** Replaces the notebook title by key presses, leaving the field focused. */
  async retitleLive(title: string) {
    await this.liveTitle().click();
    const n = (await this.liveTitle().getValue()).length;
    await browser.keys([...Array(n).fill(Key.Backspace), ...Array(n).fill(Key.Delete)]);
    await expect(this.liveTitle()).toHaveValue('');
    await browser.keys(title);
    await expect(this.liveTitle()).toHaveValue(title);
  },

  /** Pauses; the notebook swaps its single Pause button for Resume + Finish. */
  async pauseIt() {
    await this.pause().click();
    await expect(this.resume()).toBeDisplayed();
  },

  async resumeIt() {
    await this.resume().click();
    await expect(this.pause()).toBeDisplayed();
  },

  /** Mutes the mic; the button relabels itself to the muted state. */
  async muteMic() {
    await this.mute().click();
    await expect(this.mute()).toHaveText(expect.stringContaining('Mic muted'));
  },

  /** Dismisses the live-captions working-view note. */
  async dismissLiveNote() {
    await this.noteClose().click();
    await expect(this.noteClose()).not.toBeExisting();
  },

  /** Discards through the confirm dialog. `App` routes back to the library. */
  async discardIt() {
    await clickRowMenuItem('live-overflow', 'live-discard');
    await confirmDialog.acceptTyping();
    await expect(nav.item('library')).toHaveElementClass('active');
  },

  /** Hands the paused session to the background finalize cascade, which
   *  routes to the library. Only reachable while paused. */
  async finishIt() {
    await this.finish().click();
    await expect(nav.item('library')).toHaveElementClass('active');
  },
};

export const mini = {
  expand: () => tid('mini-expand'),
  pauseResume: () => tid('mini-pause-resume'),
  stop: () => tid('mini-stop'),
  record: () => tid('mini-record'),
};

export const reminder = {
  open: () => tid('reminder-open'),
  dismiss: () => tid('reminder-dismiss'),
  start: () => tid('reminder-start'),
  notNow: () => tid('reminder-not-now'),
  /** One snooze button by its slug: `30m`, `1h`, `today`. */
  snooze: (slug: string) => tid(`reminder-snooze-${slug}`),
  never: () => tid('reminder-never'),
  stop: () => tid('reminder-stop'),
  keep: () => tid('reminder-keep'),
};
