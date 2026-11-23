// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { E2E_PASSPHRASE, isNative } from '../env.ts';
import { unlock } from '../pages/gates.ts';
import { nav } from '../pages/shell.ts';
import { mini, reminder } from '../pages/record.ts';

/** The reminder popup renders whatever title `show_reminder_window` stored. */
const REMINDER_TITLE = 'Design review with Jordan';

/** Both popups are pre-built hidden at startup, so their labels are listed
 *  before anything shows them; the wait covers a slow first build. */
async function waitForWindow(label: string): Promise<void> {
  await browser.waitUntil(async () => (await browser.tauri.listWindows()).includes(label), {
    timeoutMsg: `no webview labelled "${label}" appeared`,
  });
}

describe('mini + reminder windows (native)', function () {
  before(async function () {
    if (!isNative()) {
      console.log('[e2e] browser mode has a single document; mini + reminder skipped');
      this.skip();
    }
    await unlock.ensureMain(E2E_PASSPHRASE);
  });

  // Later spec files expect the session on the main window.
  after(async function () {
    if (!isNative()) return;
    await browser.tauri.switchWindow('main');
  });

  it('mini window opens from nav and expands back to main', async () => {
    await nav.item('mini').click();
    await waitForWindow('mini');
    await browser.tauri.switchWindow('mini');
    // Nothing is recording, so the card offers its own start button.
    await expect(mini.record()).toBeDisplayed();
    await mini.expand().click();
    await browser.tauri.switchWindow('main');
    await expect(nav.item('library')).toBeDisplayed();
  });

  it('reminder window shows open/dismiss for an upcoming meeting', async () => {
    await browser.tauri.execute(
      ({ core }, title) => core.invoke('show_reminder_window', { title }),
      REMINDER_TITLE,
    );
    await waitForWindow('reminder');
    await browser.tauri.switchWindow('reminder');
    await expect(reminder.open()).toBeDisplayed();
    await expect(reminder.dismiss()).toBeDisplayed();
    await reminder.dismiss().click();
    await browser.tauri.switchWindow('main');
    await expect(nav.item('library')).toBeDisplayed();
  });
});
