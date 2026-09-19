// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { nav } from '../pages/shell.ts';
import { record } from '../pages/record.ts';
import { pressEnter } from '../pages/_shared.ts';

/** Starting a recording needs an input the capture engine can open. The
 *  browser world answers `start_recording` from its fixtures, so the live
 *  section runs there; natively it needs a real device this machine's
 *  sandboxed run has no virtual stand-in for. */
const liveCaptureAvailable = !isNative() || process.platform === 'linux';

const LIVE_TITLE = 'Live e2e';
const FINISH_TITLE = 'Finish e2e';
const RETITLED = 'Renamed mid-call';

describe('record', () => {
  before(async () => {
    await bootWorld();
    await nav.go('record');
  });

  it('pre-recording screen: title, mic picker, single-speaker toggle, start', async () => {
    await expect(record.title()).toBeDisplayed();
    await expect(record.mic()).toBeDisplayed();
    await expect(record.micLevelFill()).toBeExisting();
    await record.singleSpeaker().click();
    await record.singleSpeaker().click();
    await expect(record.start()).toBeDisplayed();
  });

  it('the pre-recording screen offers a tag box', async () => {
    await expect(record.tagInput()).toBeDisplayed();
  });

  // Enter is inert on the pre-recording title: the typed value stays put and
  // no recording starts.
  it('the pre-recording title keeps its value on Enter (issue #117 path)', async () => {
    await record.title().setValue('Enter commit');
    await pressEnter();
    await expect(record.title()).toHaveValue('Enter commit');
  });

  it('start → pause → resume → mute → discard', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    await record.begin(LIVE_TITLE);
    await expect(record.liveTitle()).toHaveValue(LIVE_TITLE);
    await record.dismissLiveNote();
    await expect(record.liveMic()).toBeDisplayed();
    await expect(record.notes()).toBeDisplayed();
    await record.pauseIt();
    await record.resumeIt();
    await record.muteMic();
    await record.discardIt();
  });

  it('the live title commits on Enter (issue #117)', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    await nav.go('record');
    await record.begin(LIVE_TITLE);
    await mockInvoke('session_meta_update', null);
    await record.retitleLive(RETITLED);
    await pressEnter();
    await browser.waitUntil(async () => (await callsOf('session_meta_update')).length === 1, {
      timeoutMsg: 'Enter on the live title never called session_meta_update',
    });
    expect((await callsOf('session_meta_update'))[0]).toMatchObject({ req: { title: RETITLED } });
    await restoreInvoke('session_meta_update');
    await record.discardIt();
  });

  it('start → pause → finish lands the session in the library', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    await nav.go('record');
    await record.begin(FINISH_TITLE);
    // Finish is only offered once capture is paused; the notebook shows a
    // single Pause button while it is running.
    await record.pauseIt();
    await record.finishIt();
  });
});
