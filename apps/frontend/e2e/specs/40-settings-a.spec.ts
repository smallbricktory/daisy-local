// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { E2E_PASSPHRASE, isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { unlock } from '../pages/gates.ts';
import { confirmDialog } from '../pages/shell.ts';
import { display, profile, recordings, settings, storage, type Section } from '../pages/settings.ts';
import { expectPicked } from '../pages/_shared.ts';

/** Every section in nav order. */
const SECTIONS: Section[] = [
  'recordings', 'display', 'tags', 'prompts', 'providers', 'integrations',
  'storage', 'voiceprints', 'calendars', 'mcp', 'profile', 'about',
];

/** The webview zoom command `uiZoom.applyZoom` drives. */
const ZOOM_CMD = 'plugin:webview|set_webview_zoom';

const CHANGE_PASSPHRASE_CMD = 'change_vault_passphrase';

const A_NEW_PASSPHRASE = 'orbit-lantern-copper-walrus-2027';

/** CSS-pixel width of the viewport. The native webview's zoom factor scales
 *  it: at 110% the same window reports fewer CSS pixels. */
const viewportWidth = () => browser.execute(() => window.innerWidth);

describe('settings: recordings, display, storage, profile', () => {
  before(async () => {
    // The browser world is licensed by default, and a licensed install binds
    // the display name to the licensee (read-only). Trial is the state that
    // renders the editable field, which is what the profile case exercises.
    await bootWorld({ license: { state: 'trial', days_left: 21 } });
  });

  it('every section tab opens', async () => {
    for (const s of SECTIONS) await settings.open(s);
  });

  it('display: theme cards select; zoom in and reset drive the webview zoom', async () => {
    await settings.open('display');
    await display.chooseTheme('dark');
    await display.chooseTheme('system');

    if (isNative()) {
      const hundredPct = await viewportWidth();
      try {
        await display.zoomInOnce();
        await browser.waitUntil(async () => (await viewportWidth()) < hundredPct, {
          timeoutMsg: 'zooming in did not shrink the CSS viewport',
        });
        await display.resetZoom();
        await browser.waitUntil(async () => (await viewportWidth()) === hundredPct, {
          timeoutMsg: 'resetting zoom did not restore the CSS viewport',
        });
      } finally {
        await display.ensureZoomReset();
      }
      return;
    }

    // No native webview behind the browser run — the command it would call is
    // the observable.
    await mockInvoke(ZOOM_CMD, null);
    try {
      await display.zoomInOnce();
      const zoomedIn = await callsOf(ZOOM_CMD);
      expect(zoomedIn.at(-1)?.value).not.toBe(1);
      await display.resetZoom();
      const afterReset = await callsOf(ZOOM_CMD);
      expect(afterReset.at(-1)?.value).toBe(1);
    } finally {
      await display.ensureZoomReset();
      await restoreInvoke(ZOOM_CMD);
    }
  });

  it('recordings: the mic and level controls show; debug level round-trips', async () => {
    await settings.open('recordings');
    await expect(recordings.mic()).toBeDisplayed();
    await expect(recordings.aec()).toBeDisplayed();
    await expect(recordings.aiProvider()).toBeDisplayed();
    await expect(recordings.openLogs()).toBeDisplayed();
    await expect(recordings.speechLevelDevice()).toBeDisplayed();
    await expect(recordings.calibrate()).toBeDisplayed();

    await recordings.setDebugLevel('basic');
    await settings.open('display');
    await settings.open('recordings');
    await expectPicked(recordings.debugLevel(), 'basic');
    await recordings.setDebugLevel('off');
  });

  it('recordings: the noise switch flips and flips back', async () => {
    await settings.open('recordings');
    await recordings.flipToggle('reduce-background-noise');
    await recordings.flipToggle('reduce-background-noise');
  });

  it('recordings: the manual speech level reveals its slider', async function () {
    await settings.open('recordings');
    if (!(await recordings.manual().isEnabled())) {
      console.log('[e2e] no input device to hold a speech level; override section skipped');
      return this.skip();
    }
    await recordings.manual().click();
    await expect(recordings.slider()).toBeDisplayed();
    await recordings.manual().click();
    await expect(recordings.slider()).not.toBeExisting();
  });

  it('storage: clear asks for DELETE and cancel keeps the recordings', async function () {
    await settings.open('storage');
    if (!(await storage.clear().isEnabled())) {
      console.log('[e2e] nothing deletable in this profile; clear section skipped');
      return this.skip();
    }
    await storage.openClear();
    await expect(confirmDialog.accept()).toBeDisabled();
    await storage.typeConfirmWord('delete');
    await expect(confirmDialog.accept()).toBeDisabled();
    await storage.typeConfirmWord('DELETE');
    await expect(confirmDialog.accept()).toBeEnabled();
    await storage.cancelClear();
  });

  it('profile: the display name saves and the profile buttons are offered', async () => {
    await settings.open('profile');
    await profile.setDisplayName('Alex Q');
    await expect(profile.openDir()).toBeDisplayed();
    await expect(profile.switchProfile()).toBeDisplayed();
    await expect(profile.move()).toBeDisplayed();
    await expect(profile.openWizard()).toBeDisplayed();
  });

  it('profile: locking the vault returns to Unlock', async () => {
    await unlock.lock();
    // Both modes carry the lock into the tests below, so both unlock again.
    await unlock.withPassphrase(E2E_PASSPHRASE);
  });

  it('profile: a mismatched confirmation never reaches the vault', async () => {
    await settings.open('profile');
    // Mocked so a broken form cannot re-key the vault the rest of the run
    // depends on: the form shows the error, and the command stays uncalled.
    await mockInvoke(CHANGE_PASSPHRASE_CMD, null);
    try {
      await profile.beginPassphraseChange();
      await profile.attemptInvalidPassphraseChange(
        E2E_PASSPHRASE, A_NEW_PASSPHRASE, 'mismatch', "don't match",
      );
      expect(await callsOf(CHANGE_PASSPHRASE_CMD)).toEqual([]);
    } finally {
      await profile.ensurePassphraseChangeClosed();
      await restoreInvoke(CHANGE_PASSPHRASE_CMD);
    }
  });
});
