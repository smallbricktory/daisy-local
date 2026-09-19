// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { E2E_PASSPHRASE, isNative } from '../env.ts';
import { ensureWorld } from '../fixtures/install.ts';
import { mockInvoke, mockInvokeReject, restoreInvoke } from '../fixtures/invoke.ts';
import { unlock } from '../pages/gates.ts';
import { typeIntoEl, pressEnter } from '../pages/_shared.ts';
import { nav } from '../pages/shell.ts';

/** The file-picker command "Open a different profile…" calls. */
const DIALOG_OPEN = 'plugin:dialog|open';
/** A folder a spec picks with the mocked dialog. */
const A_FOLDER = '/Volumes/Backup/daisy-profile';
/** The fictional computer that owns the machine-mode vault below — never
 *  this test run's own `this_machine` name. */
const OWNER = 'Rowan-Mac';

describe('unlock', () => {
  before(async () => {
    await ensureWorld({ phase: { eula: true, consent: true, vaultExists: true, unlocked: false } });
    if (isNative()) {
      await unlock.ensureMain(E2E_PASSPHRASE);
      await unlock.lock();
    }
  });

  it('wrong passphrase shows an error and stays locked', async () => {
    await typeIntoEl(unlock.passphrase(), 'not-the-passphrase-at-all-no');
    await unlock.submit().click();
    await expect(unlock.error()).toBeDisplayed();
    await expect(unlock.passphrase()).toBeDisplayed();
  });

  it('show-passphrase toggles the input type', async () => {
    await unlock.show().click();
    await expect(unlock.passphrase()).toHaveAttribute('type', 'text');
    await unlock.show().click();
    await expect(unlock.passphrase()).toHaveAttribute('type', 'password');
  });

  it('forgot → reset dialog → cancel returns to unlock', async () => {
    await unlock.forgot().click();
    await expect(unlock.resetText()).toBeDisplayed();
    await expect(unlock.resetConfirm()).toBeDisabled();
    await unlock.resetCancel().click();
    await expect(unlock.resetText()).not.toBeExisting();
    await expect(unlock.passphrase()).toBeDisplayed();
  });

  it('forgot → reset dialog → a click on the backdrop closes it', async () => {
    await unlock.forgot().click();
    await expect(unlock.resetModal()).toBeDisplayed();
    await expect(unlock.resetBackdrop()).toBeExisting();
    await unlock.closeResetByBackdrop();
    await expect(unlock.passphrase()).toBeDisplayed();
  });

  it('Enter submits the right passphrase', async () => {
    await typeIntoEl(unlock.passphrase(), E2E_PASSPHRASE);
    await pressEnter();
    await expect(nav.item('library')).toBeDisplayed();
  });
});

// A machine-mode vault still locked here did not open with this computer's
// key: it belongs to another one. Browser-mode only — there is no way to put
// the real backend into this state from a single native profile.
describe('unlock — machine-mode vault from another computer', () => {
  before(function () {
    if (isNative()) this.skip();
  });

  before(async () => {
    await ensureWorld({
      phase: { eula: true, consent: true, vaultExists: true, unlocked: false },
      vaultTrust: { kind: 'machine', trusted_on: OWNER, this_machine: 'studio-mac' },
    });
  });

  it('names the owning computer instead of the plain passphrase prompt', async () => {
    await expect(unlock.elsewhere()).toBeDisplayed();
    await expect(unlock.elsewhere()).toHaveText(expect.stringContaining(`only on ${OWNER}`));
  });

  it('the reset dialog warns the owning computer is affected too', async () => {
    await unlock.forgot().click();
    await expect(unlock.resetShared()).toBeDisplayed();
    await expect(unlock.resetShared()).toHaveText(expect.stringContaining(`${OWNER} loses the same keys`));
    await unlock.resetCancel().click();
    await expect(unlock.resetText()).not.toBeExisting();
  });

  it('opening a different profile shows the error when the switch fails', async () => {
    await mockInvoke(DIALOG_OPEN, A_FOLDER);
    await mockInvokeReject('switch_profile', 'that folder has no Daisy profile in it');
    try {
      await unlock.otherProfile().click();
      await expect(unlock.switchError()).toBeDisplayed();
      await expect(unlock.switchError()).toHaveText(expect.stringContaining('that folder has no Daisy profile'));
    } finally {
      await restoreInvoke(DIALOG_OPEN);
      await restoreInvoke('switch_profile');
    }
  });
});

describe('unlock — machine-mode vault whose own key fails', () => {
  before(function () {
    if (isNative()) this.skip();
  });

  before(async () => {
    await ensureWorld({
      phase: { eula: true, consent: true, vaultExists: true, unlocked: false },
      vaultTrust: { kind: 'machine', trusted_on: 'studio-mac', this_machine: 'studio-mac' },
    });
  });

  it("says this computer's key no longer opens the profile", async () => {
    await expect(unlock.keyLost()).toBeDisplayed();
    await expect(unlock.keyLost()).toHaveText(expect.stringContaining("This computer's key no longer opens this profile"));
    await expect(unlock.elsewhere()).not.toBeExisting();
  });
});
