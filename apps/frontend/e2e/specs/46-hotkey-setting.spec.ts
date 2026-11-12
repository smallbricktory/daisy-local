// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { mockInvokeReject, restoreInvoke } from '../fixtures/invoke.ts';
import { recordings, settings } from '../pages/settings.ts';
import { nav, toast } from '../pages/shell.ts';

/** WebDriver's modifier codepoints. */
const CONTROL = '';
const ALT = '';

const UPDATE_CMD = 'update_machine_settings';

describe('settings: the start-recording shortcut', () => {
  before(async () => {
    await bootWorld({});
  });

  it('shows the stored combination as key caps', async () => {
    await settings.open('recordings');
    await expect(recordings.hotkey()).toBeDisplayed();
    await expect(recordings.hotkey()).toHaveAttribute('data-accel', 'Control+Alt+R');
    await expect(recordings.hotkey()).toHaveText(expect.stringContaining('R'));
    await expect(recordings.hotkeyClear()).toBeDisplayed();
  });

  it('captures a new combination and clears back to off', async function () {
    if (isNative()) return this.skip();
    await settings.open('recordings');
    await recordings.captureHotkey([CONTROL, ALT, 'p'], 'Control+Alt+P');

    await recordings.hotkeyClear().click();
    await expect(recordings.hotkey()).toHaveAttribute('data-accel', 'off');
    await expect(recordings.hotkey()).toHaveText('Off');
    await expect(recordings.hotkeyClear()).not.toBeExisting();

    await recordings.captureHotkey([CONTROL, ALT, 'r'], 'Control+Alt+R');
  });

  it('Escape leaves capture mode with the combination untouched', async function () {
    if (isNative()) return this.skip();
    await settings.open('recordings');
    await recordings.hotkey().click();
    await expect(recordings.hotkey()).toHaveText('Press a shortcut, Esc to cancel');
    await browser.keys('Escape');
    await expect(recordings.hotkey()).toHaveAttribute('data-accel', 'Control+Alt+R');
  });

  it('renders the backend refusal under the field and keeps the old value', async function () {
    if (isNative()) return this.skip();
    await settings.open('recordings');
    await mockInvokeReject(UPDATE_CMD, {
      kind: 'Config',
      code: 'DAISY-E014',
      message: 'Another app already uses Control+Alt+P. Pick a different combination.',
      friendly: 'Another app already uses Control+Alt+P. Pick a different combination.',
    });
    try {
      await recordings.hotkey().click();
      await browser.keys([CONTROL, ALT, 'p']);
      await expect(recordings.hotkeyError())
        .toHaveText(expect.stringContaining('Another app already uses Control+Alt+P. Pick a different combination.'));
      await expect(recordings.hotkey()).toHaveAttribute('data-accel', 'Control+Alt+R');
    } finally {
      await restoreInvoke(UPDATE_CMD);
    }
  });

  it('a hotkey start navigates to the recording screen from anywhere', async () => {
    await nav.go('settings');
    await emitAppEvent('hotkey:started', null);
    await expect(nav.item('record')).toHaveElementClass('active');
  });

  it('a hotkey start with no microphone lands on the picker with a toast', async () => {
    await nav.go('library');
    await emitAppEvent('hotkey:no-mic', null);
    await expect(nav.item('record')).toHaveElementClass('active');
    await expect(toast.byId('hotkey-no-mic')).toBeDisplayed();
    await expect(toast.byId('hotkey-no-mic')).toHaveText(expect.stringContaining('No microphone found'));
  });

  it('a hotkey start failure toasts the reason', async () => {
    await nav.go('library');
    await emitAppEvent('hotkey:start-failed', 'Microphone is busy.');
    await expect(toast.byId('hotkey-start-failed')).toBeDisplayed();
    await expect(toast.byId('hotkey-start-failed')).toHaveText(expect.stringContaining('Recording did not start'));
  });
});
