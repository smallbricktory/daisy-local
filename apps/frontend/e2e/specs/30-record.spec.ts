// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, clearCalls, mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { nav, toast } from '../pages/shell.ts';
import { record } from '../pages/record.ts';
import { pressEnter, pressEscape, tid } from '../pages/_shared.ts';

/** Starting a recording needs an input the capture engine can open. The
 *  browser world answers `start_recording` from its fixtures; natively the
 *  live section runs on Linux only. */
const liveCaptureAvailable = !isNative() || process.platform === 'linux';

const LIVE_TITLE = 'Live e2e';
const FINALIZE = 'recording_finalize_and_summarize';
const FINISH_REQUEST = 'daisy://recording/finish';
const FINISH_TITLE = 'Finish e2e';
const RETITLED = 'Renamed mid-call';

describe('record', () => {
  before(async () => {
    await bootWorld();
    await nav.go('record');
  });

  // Discards a recording left running by a failed case.
  afterEach(async function () {
    if (this.currentTest?.state !== 'failed') return;
    await nav.go('record');
    if (await record.overflow().isExisting()) await record.discardIt();
  });

  it('pre-recording screen: title, mic picker, single-speaker toggle, start', async () => {
    await expect(record.title()).toBeDisplayed();
    await expect(record.mic()).toBeDisplayed();
    // A missing meter names the capture error from the fallback's tooltip.
    const unavailable = record.micLevelUnavailable();
    if (await unavailable.isExisting()) {
      throw new Error(`mic level meter unavailable: ${await unavailable.getAttribute('title')}`);
    }
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
  it('the pre-recording title keeps its value on Enter', async () => {
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

  it('the live screen follows a pause and resume made elsewhere, and its own pause holds while the snapshot lags', async function () {
    if (!liveCaptureAvailable) return this.skip();
    await nav.go('record');
    await record.begin('Follow e2e');
    // Each event below is the backend's own snapshot with the state and clock changed.
    const live = await browser.execute(() => {
      const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string) => Promise<unknown> } };
      return w.__TAURI_INTERNALS__.invoke('recording_snapshot');
    }) as Record<string, unknown> | null;
    expect(live).not.toBeNull();
    const snap = (state: 'recording' | 'paused') => ({
      ...live, state,
      recorded_ms: 60_000, clock_running_since_unix_ms: state === 'recording' ? Date.now() : null,
    });
    // This window's pause resolves before its snapshot event arrives.
    await mockInvoke('pause_recording', 'paused');
    try {
      await record.pauseIt();
      await browser.pause(500);
      await expect(record.resume()).toBeDisplayed();
    } finally {
      await restoreInvoke('pause_recording');
    }
    await emitAppEvent('recording:snapshot', snap('paused'));
    await expect(record.resume()).toBeDisplayed();
    // The computer woke and the backend resumed the recording.
    await emitAppEvent('recording:snapshot', snap('recording'));
    await expect(record.pause()).toBeDisplayed();
    // The computer slept and the backend paused the recording.
    await emitAppEvent('recording:snapshot', snap('paused'));
    await expect(record.resume()).toBeDisplayed();
    await emitAppEvent('recording:snapshot', snap('recording'));
    await expect(record.pause()).toBeDisplayed();
    await record.discardIt();
  });

  it('the live title commits on Enter', async function () {
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

  it('a recording started from the meeting reminder takes over the open picker', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    await nav.go('record');
    await expect(record.start()).toBeDisplayed();
    // The reminder window's Start button, sent from this window.
    await browser.execute(() => {
      const w = window as unknown as { __TAURI_INTERNALS__: { invoke: (c: string, a: unknown) => Promise<unknown> } };
      return w.__TAURI_INTERNALS__.invoke('reminder_action', { choice: 'start' });
    });
    await expect(record.pause()).toBeDisplayed();
    await record.discardIt();
  });

  it('the live tag box stays usable in a narrow window and its menu draws over every pane', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    const { width, height } = await browser.getWindowSize();
    await nav.go('record');
    await record.begin(LIVE_TITLE);
    try {
      await browser.setWindowSize(560, 560);
      const box = await record.tagInput().getSize();
      expect(box.width).toBeGreaterThan(90);
      await record.tagInput().click();
      // Every row inside the menu's own scroll box is the topmost element at
      // its centre: nothing paints over it and no pane clips it.
      const covered = await browser.execute(() => {
        const menu = document.querySelector('.tag-combobox__pop');
        if (!menu) return ['no menu'];
        const m = menu.getBoundingClientRect();
        const rows = [...menu.querySelectorAll('.menu__item')].filter((row) => {
          const r = row.getBoundingClientRect();
          return r.top + r.height / 2 < m.bottom;
        });
        return rows.length === 0 ? ['no rows'] : rows.flatMap((row) => {
          const r = row.getBoundingClientRect();
          const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
          return hit && row.contains(hit) ? [] : [row.textContent ?? ''];
        });
      });
      expect(covered).toEqual([]);
    } finally {
      await browser.setWindowSize(width, height);
    }
    await pressEscape();
    await record.discardIt();
  });

  it('the recording toast hides on the recording screen, and no toast covers the live control bar', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    const { width, height } = await browser.getWindowSize();
    await nav.go('record');
    await record.begin(LIVE_TITLE);
    // The recording toast shows only away from the recording screen.
    await expect(tid('toast-session-status')).not.toBeDisplayed();
    await nav.go('library');
    await expect(tid('toast-session-status')).toBeDisplayed();
    await nav.go('record');
    await expect(tid('toast-session-status')).not.toBeDisplayed();
    // A warning toast is up while the bar's controls are checked.
    await emitAppEvent('audio-warning', { kind: 'system-audio-lost' });
    await expect(toast.byId('audio-system-lost')).toBeDisplayed();
    // Each control in the bar is the topmost element at its centre.
    const coveredControls = () => browser.execute(() =>
      [...document.querySelectorAll('.notebook__controls [data-testid]')].flatMap((el) => {
        const r = el.getBoundingClientRect();
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        return hit && el.contains(hit) ? [] : [el.getAttribute('data-testid') ?? ''];
      }));
    try {
      expect(await coveredControls()).toEqual([]);
      await browser.setWindowSize(560, 560);
      await browser.waitUntil(async () => (await browser.execute(() => window.innerWidth)) < 600);
      expect(await coveredControls()).toEqual([]);
    } finally {
      await browser.setWindowSize(width, height);
    }
    await record.discardIt();
    await expect(toast.byId('audio-system-lost')).not.toBeExisting();
  });

  it('start → pause → finish lands the session in the library', async function () {
    if (!liveCaptureAvailable) {
      console.log('[e2e] no virtual input on this OS; live section skipped');
      return this.skip();
    }
    await nav.go('record');
    await record.begin(FINISH_TITLE);
    // Finish is offered once capture is paused.
    await record.pauseIt();
    await record.finishIt();
  });

  // The mini window's Stop and the meeting-ended reminder's Stop reach the
  // main window as this one request.
  it('a finish request from another window processes the session and leaves the record screen', async function () {
    if (!liveCaptureAvailable) return this.skip();
    await nav.go('record');
    await record.begin('Finish from mini');
    await mockInvoke(FINALIZE, { status: 'completed', summary: null });
    try {
      await clearCalls();
      await emitAppEvent(FINISH_REQUEST, null);
      await expect(nav.item('library')).toHaveElementClass('active');
      await browser.waitUntil(async () => (await callsOf(FINALIZE)).length === 1, {
        timeoutMsg: 'the finish request never started the finalize cascade',
      });
    } finally {
      await restoreInvoke(FINALIZE);
    }
  });

  it('a finish request while another screen is open still processes the session', async function () {
    if (!liveCaptureAvailable) return this.skip();
    await nav.go('record');
    await record.begin('Finish from elsewhere');
    await nav.go('library');
    await mockInvoke(FINALIZE, { status: 'completed', summary: null });
    try {
      await clearCalls();
      await emitAppEvent(FINISH_REQUEST, null);
      await browser.waitUntil(async () => (await callsOf(FINALIZE)).length === 1, {
        timeoutMsg: 'the finish request never started the finalize cascade',
      });
    } finally {
      await restoreInvoke(FINALIZE);
    }
  });

  it('discarding a recording starts no finalize', async function () {
    if (!liveCaptureAvailable) return this.skip();
    await nav.go('record');
    await record.begin('Discard, not process');
    await mockInvoke(FINALIZE, { status: 'completed', summary: null });
    try {
      await clearCalls();
      await record.discardIt();
      await browser.pause(500);
      expect(await callsOf(FINALIZE)).toHaveLength(0);
    } finally {
      await restoreInvoke(FINALIZE);
    }
  });
});
