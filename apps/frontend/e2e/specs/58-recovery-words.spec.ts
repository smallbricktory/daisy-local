// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The recovery key: shown once after the first unlock, shown again from
// Settings after the passphrase, and asked for when recordings stay locked.
import { expect } from '@wdio/globals';
import { E2E_PASSPHRASE, isNative } from '../env.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { ensureWorld } from '../fixtures/install.ts';
import { recovery } from '../pages/gates.ts';
import { typeInto } from '../pages/_shared.ts';
import { nav } from '../pages/shell.ts';
import { nerdMode, profile, settings } from '../pages/settings.ts';

// Fixture words and the world's passphrase: the backend itself is covered by
// its Rust tests, so these specs run against the fixture world only.
const WORDS = ['orbit', 'cabin', 'lemon', 'river', 'falcon'];

describe('recovery words', function () {
  before(function () {
    if (isNative()) this.skip();
  });

  describe('first unlock', () => {
    before(async () => {
      await ensureWorld();
    });

    it('recovery words show once after first unlock', async () => {
      await expect(nav.item('library')).toBeDisplayed();
      await emitAppEvent('encryption:words_ready', WORDS);
      await expect(recovery.dialog()).toBeDisplayed();
      const shown = await recovery.words();
      expect(shown.length).toBe(5);
      for (const [i, w] of WORDS.entries()) await expect(shown[i]).toHaveText(w);
      await expect(recovery.done()).toBeDisabled();
      await recovery.saved().click();
      await expect(recovery.done()).toBeEnabled();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });
  });

  describe('app start with the words still pending', () => {
    before(async () => {
      await ensureWorld({
        vaultTrust: { kind: 'machine', trusted_on: 'studio-mac', this_machine: 'studio-mac' },
        encryption: { content_key: 'ok', words_pending: true },
      });
    });

    it('shows the words without an event', async () => {
      await expect(recovery.dialog()).toBeDisplayed();
      await expect(await recovery.words()).toHaveLength(5);
      await recovery.saved().click();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });
  });

  describe('recordings locked', () => {
    before(async () => {
      await ensureWorld({ encryption: { content_key: 'unopenable', words_pending: false } });
    });

    it('the screen can be left; the library offers the words', async () => {
      await expect(recovery.entry()).toBeDisplayed();
      await recovery.entryLater().click();
      await expect(recovery.entry()).not.toBeExisting();
      await expect(nav.item('record')).toBeDisplayed();
      await expect(recovery.libraryLocked()).toBeDisplayed();
      await recovery.libraryEnter().click();
      await expect(recovery.entry()).toBeDisplayed();
    });

    it('settings offers no recovery key to show while none is open', async () => {
      await recovery.entryLater().click();
      await settings.open('profile');
      await expect(profile.vaultChange()).toBeDisplayed();
      await expect(profile.vaultShowRecovery()).not.toBeExisting();
      await nav.go('library');
      await recovery.libraryEnter().click();
      await expect(recovery.entry()).toBeDisplayed();
    });

    it('wrong recovery entry shows the single error', async () => {
      await expect(recovery.entry()).toBeDisplayed();
      await typeInto('recovery-entry-input', 'abandon zoo ladder act quantum');
      await recovery.entrySubmit().click();
      await expect(recovery.entryError()).toHaveText('The recovery key you entered is incorrect.');
      await expect(recovery.entry()).toBeDisplayed();
    });

    it('the right words reopen the library', async () => {
      await typeInto('recovery-entry-input', WORDS.join(' '));
      await recovery.entrySubmit().click();
      await expect(nav.item('library')).toBeDisplayed();
      await expect(recovery.entry()).not.toBeExisting();
      await expect(recovery.libraryLocked()).not.toBeExisting();
    });
  });

  describe('content key still syncing in', () => {
    before(async () => {
      await ensureWorld({ encryption: { content_key: 'missing', words_pending: false } });
    });

    it('the library waits for sync without asking for the words', async () => {
      await expect(recovery.libraryWaiting()).toBeDisplayed();
      await expect(recovery.entry()).not.toBeExisting();
    });

    it('the words open it before the sync does', async () => {
      await recovery.libraryEnter().click();
      await expect(recovery.entry()).toBeDisplayed();
      await typeInto('recovery-entry-input', WORDS.join(' '));
      await recovery.entrySubmit().click();
      await expect(nav.item('library')).toBeDisplayed();
      await expect(recovery.libraryWaiting()).not.toBeExisting();
    });
  });

  describe('lost recovery key', () => {
    const OLD_WORDS = ['orbit', 'cabin', 'lemon', 'river', 'falcon'];
    const NEW_WORDS = ['harbor', 'velvet', 'quartz', 'meadow', 'copper'];
    const LOCKED_ID = 's-locked-1';

    before(async () => {
      await ensureWorld({
        encryption: { content_key: 'unopenable', words_pending: false },
        recoveryWords: OLD_WORDS,
        lockedSessions: [{
          session_id: LOCKED_ID, created_at_unix_seconds: 1_700_000_000, finalized_at_unix_seconds: null,
          duration_seconds: null, title: null, tag_ids: [], has_transcript: false, has_dedup: false,
          has_summary: false, locked: true,
        }],
      });
    });

    it('starting a new key asks first, then shows the new words', async () => {
      await expect(recovery.entry()).toBeDisplayed();
      await recovery.startNew().click();
      await expect(recovery.startNewConfirm()).toBeDisplayed();
      await recovery.startNewCancel().click();
      await expect(recovery.startNewConfirm()).not.toBeExisting();
      await recovery.startNew().click();
      await recovery.startNewGo().click();
      await expect(recovery.dialog()).toBeDisplayed();
      const shown = await recovery.words();
      expect(shown.length).toBe(5);
      for (const [i, w] of NEW_WORDS.entries()) await expect(shown[i]).toHaveText(w);
      await recovery.saved().click();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });

    it('the old recordings show locked, with a date and no title', async () => {
      await expect(nav.item('library')).toBeDisplayed();
      await expect(recovery.lockedRow(LOCKED_ID)).toBeDisplayed();
      await expect(recovery.lockedRow(LOCKED_ID)).not.toHaveText('Untitled');
      await expect(recovery.lockedRowEnter(LOCKED_ID)).toBeDisplayed();
    });

    it('the old words reopen them and the library reloads', async () => {
      await recovery.lockedRowEnter(LOCKED_ID).click();
      await expect(recovery.entry()).toBeDisplayed();
      await expect(recovery.startNew()).not.toBeExisting();
      await typeInto('recovery-entry-input', OLD_WORDS.join(' '));
      await recovery.entrySubmit().click();
      await expect(nav.item('library')).toBeDisplayed();
      await expect(recovery.lockedRow(LOCKED_ID)).not.toBeExisting();
    });
  });

  describe('a recording whose key has not synced in', () => {
    const WAITING_ID = 's-waiting-1';

    before(async () => {
      await ensureWorld({
        lockedSessions: [{
          session_id: WAITING_ID, created_at_unix_seconds: 1_700_000_100, finalized_at_unix_seconds: null,
          duration_seconds: null, title: null, tag_ids: [], has_transcript: false, has_dedup: false,
          has_summary: false, locked: true, waiting_for_key: true,
        }],
      });
    });

    it('waits for sync without asking for the words', async () => {
      await nav.go('library');
      await expect(recovery.lockedRowWaiting(WAITING_ID)).toBeDisplayed();
      await expect(recovery.lockedRowEnter(WAITING_ID)).not.toBeExisting();
    });
  });

  describe('settings', () => {
    before(async () => {
      await ensureWorld();
      await settings.open('profile');
    });

    it('cancel closes the passphrase prompt', async () => {
      await profile.vaultShowRecovery().click();
      await expect(recovery.passphrase()).toBeDisplayed();
      await recovery.cancel().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });

    it('settings shows words after passphrase', async () => {
      await profile.vaultShowRecovery().click();
      await expect(recovery.passphrase()).toBeDisplayed();
      await typeInto('recovery-passphrase', 'not the passphrase at all, no');
      await recovery.show().click();
      await expect(recovery.showError()).toBeDisplayed();
      await expect(recovery.words()).toBeElementsArrayOfSize(0);
      await typeInto('recovery-passphrase', E2E_PASSPHRASE);
      await recovery.show().click();
      await expect(recovery.words()).toBeElementsArrayOfSize(5);
      await recovery.saved().click();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });
  });

  // Each describe installs a fresh world, which turns Nerd Mode back on.
  describe('settings with Nerd Mode off', () => {
    before(async () => {
      await ensureWorld();
      await nerdMode.set(false);
      await settings.open('profile');
    });

    it('the recovery key is offered while the vault controls stay hidden', async () => {
      await expect(profile.vaultShowRecovery()).toBeDisplayed();
      await expect(profile.vaultChange()).not.toBeExisting();
    });

    it('the words show after the passphrase', async () => {
      await profile.vaultShowRecovery().click();
      await typeInto('recovery-passphrase', 'not the passphrase at all, no');
      await recovery.show().click();
      await expect(recovery.showError()).toBeDisplayed();
      await expect(recovery.words()).toBeElementsArrayOfSize(0);
      await typeInto('recovery-passphrase', E2E_PASSPHRASE);
      await recovery.show().click();
      await expect(recovery.words()).toBeElementsArrayOfSize(5);
      await recovery.saved().click();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });
  });

  describe('settings with Nerd Mode off on a machine-key vault', () => {
    before(async () => {
      await ensureWorld({ vaultTrust: { kind: 'machine', trusted_on: 'studio-mac', this_machine: 'studio-mac' } });
      await nerdMode.set(false);
      await settings.open('profile');
    });

    it('the words show without a passphrase', async () => {
      await expect(profile.vaultAddPassphrase()).not.toBeExisting();
      await profile.vaultShowRecovery().click();
      await expect(recovery.words()).toBeElementsArrayOfSize(5);
      await expect(recovery.passphrase()).not.toBeExisting();
      await recovery.saved().click();
      await recovery.done().click();
      await expect(recovery.dialog()).not.toBeExisting();
    });
  });

  describe('dropping the passphrase', () => {
    before(async () => {
      await ensureWorld();
      await settings.open('profile');
    });

    it('a wrong passphrase keeps the vault on its passphrase', async () => {
      await profile.vaultDrop().click();
      await typeInto('vault-drop-current', 'not the passphrase at all, no');
      await profile.vaultDropSubmit().click();
      await expect(profile.vaultChangeError()).toBeDisplayed();
      await profile.vaultDropCancel().click();
      await expect(profile.vaultChange()).toBeDisplayed();
      await profile.vaultShowRecovery().click();
      await expect(recovery.passphrase()).toBeDisplayed();
      await recovery.cancel().click();
    });

    it('the right passphrase trusts this machine', async () => {
      await profile.vaultDrop().click();
      await typeInto('vault-drop-current', E2E_PASSPHRASE);
      await profile.vaultDropSubmit().click();
      await expect(profile.vaultAddPassphrase()).toBeDisplayed();
    });
  });
});
