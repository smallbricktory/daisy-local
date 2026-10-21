// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the pre-main gates: EULA, consent, first-run wizard,
// passphrase creation, unlock, and the profile-binding gate.
import { browser, expect } from '@wdio/globals';
import { clickScrim, tid, tids, typeInto } from './_shared.ts';
import { nav } from './shell.ts';
import { localModel, nerdMode, profile, settings } from './settings.ts';

/** Registry ids first-run setup queues when a model is missing. */
const AI_MODEL_ID = 'gemma-4-e4b-it-q4';
const ASR_ENGINE_IDS = ['nemotron-speech-streaming-en-0.6b-560ms-int8', 'speechanalyzer'];

export const eula = {
  tab: (k: 'tos' | 'privacy') => tid(`eula-tab-${k}`),
  agree: () => tid('eula-agree'),
  accept: () => tid('eula-accept'),
  async acceptAll() {
    await expect(this.accept()).toBeDisabled();
    await this.agree().click();
    await expect(this.accept()).toBeEnabled();
    await this.accept().click();
    await expect(this.accept()).not.toBeExisting();
  },
};

export const consent = {
  item: (k: string) => tid(`consent-item-${k}`),
  accept: () => tid('consent-accept'),
  async acceptAll() {
    await expect(this.accept()).toBeDisabled();
    for (const el of await tids('consent-item-')) await el.click();
    await expect(this.accept()).toBeEnabled();
    await this.accept().click();
    await expect(this.accept()).not.toBeExisting();
  },
};

export const wizard = {
  back: () => tid('wizard-back'),
  toSettings: () => tid('wizard-to-settings'),
  getStarted: () => tid('wizard-get-started'),
  introName: () => tid('wizard-intro-name'),
  introPathDefault: () => tid('wizard-intro-path-default'),
  introPathCustom: () => tid('wizard-intro-path-custom'),
  introPathChange: () => tid('wizard-intro-path-change'),
  introPathBrowse: () => tid('wizard-intro-path-browse'),
  introAutoUpdate: () => tid('wizard-intro-auto-update'),
  introContinue: () => tid('wizard-intro-continue'),
  micSelect: () => tid('wizard-mic-select'),
  micContinue: () => tid('wizard-mic-continue'),
  namesOn: () => tid('wizard-names-on'),
  namesOff: () => tid('wizard-names-off'),
  namesContinue: () => tid('wizard-names-continue'),
  finish: () => tid('wizard-finish'),
  planRow: (k: 'ai' | 'transcription' | 'recordings') => tid(`plan-row-${k}`),
  planStatus: (k: 'ai' | 'transcription') => tid(`plan-status-${k}`),
  planRetry: () => tid('plan-retry'),
  /** Welcome → name → mic → plan → main. Leaves `auto_update_check` false in the profile it creates. */
  async completeFirstRun(name = 'Alex') {
    await this.toMicStep(name);
    await this.finishFromMic();
  },
  /** Welcome → name, stopping on the microphone step. */
  async toMicStep(name = 'Alex') {
    await this.getStarted().click();
    await typeInto('wizard-intro-name', name);
    if (await this.introAutoUpdate().isSelected()) await this.introAutoUpdate().click();
    await expect(this.introAutoUpdate()).not.toBeSelected();
    await this.introContinue().click();
    await expect(this.micContinue()).toBeDisplayed();
  },
  /** Mic → speaker names → plan → main. Asserts the three plan rows are
   *  populated before finishing. */
  async finishFromMic() {
    await this.micContinue().click();
    await this.namesContinue().click();
    await expect(this.planRow('ai')).toBeDisplayed();
    await expect(this.planRow('transcription')).toBeDisplayed();
    await expect(this.planRow('recordings')).toBeDisplayed();
    await this.finish().click();
    await expect(nav.item('library')).toBeDisplayed();
  },
};

export const unlock = {
  passphrase: () => tid('unlock-passphrase'),
  show: () => tid('unlock-show'),
  submit: () => tid('unlock-submit'),
  forgot: () => tid('unlock-forgot'),
  error: () => tid('unlock-error'),
  /** Shown instead of the plain "enter your passphrase" copy when the vault
   *  is machine-mode and belongs to another computer; names that computer. */
  elsewhere: () => tid('unlock-elsewhere'),
  /** This computer's own machine key no longer opens the profile. */
  keyLost: () => tid('unlock-key-lost'),
  /** Opens a different profile folder via the OS picker. */
  otherProfile: () => tid('unlock-other-profile'),
  /** Shown when switching to the picked profile folder fails. */
  switchError: () => tid('unlock-switch-error'),
  resetBackdrop: () => tid('unlock-reset-backdrop'),
  resetModal: () => tid('unlock-reset-modal'),
  resetText: () => tid('unlock-reset-text'),
  resetConfirm: () => tid('unlock-reset-confirm'),
  resetCancel: () => tid('unlock-reset-cancel'),
  /** Warns a machine-mode-elsewhere reset also affects the owning computer. */
  resetShared: () => tid('unlock-reset-shared'),
  /** A click on the backdrop, off the modal, closes the reset dialog. */
  async closeResetByBackdrop() {
    await clickScrim('unlock-reset-backdrop');
    await expect(this.resetModal()).not.toBeExisting();
  },
  async withPassphrase(p: string) {
    await expect(this.passphrase()).toBeDisplayed();
    await typeInto('unlock-passphrase', p);
    await this.submit().click();
    await expect(nav.item('library')).toBeDisplayed();
  },
  /** Leaves the app on the main shell wherever it starts: unlocked, locked,
   *  a fresh sandbox still at the EULA gate (single-spec native runs boot
   *  an empty profile — the whole first-run walk happens here), or the
   *  wizard intro an earlier spec stopped at after the EULA and consent. */
  async ensureMain(p: string) {
    await browser.waitUntil(
      async () =>
        (await this.passphrase().isExisting()) ||
        (await nav.item('library').isExisting()) ||
        (await eula.accept().isExisting()) ||
        (await wizard.getStarted().isExisting()),
      { timeoutMsg: 'no unlock screen, main shell, EULA gate, or wizard intro appeared' },
    );
    if (await eula.accept().isExisting()) {
      await eula.acceptAll();
      await consent.acceptAll();
    }
    if (await wizard.getStarted().isExisting()) {
      await wizard.completeFirstRun();
      // Cancels the AI model and transcription engine downloads the plan page queued.
      await settings.open('providers');
      if (await localModel.cancel(AI_MODEL_ID).isExisting()) await localModel.cancel(AI_MODEL_ID).click();
      for (const id of ASR_ENGINE_IDS) {
        if (await asrModels.cancel(id).isExisting()) await asrModels.cancel(id).click();
      }
      await nav.go('library');
      // First run creates a machine-key vault; the unlock specs need a
      // passphrase on it. The Vault group sits behind Nerd Mode.
      await nerdMode.set(true);
      await profile.addPassphrase(p);
      return;
    }
    if (await this.passphrase().isExisting()) {
      await this.withPassphrase(p);
      return;
    }
    await expect(nav.item('library')).toBeDisplayed();
  },
  /** Locks through the real UI: Settings → Profile → Lock vault now. */
  async lock() {
    await settings.open('profile');
    await profile.vaultLock().click();
    await expect(this.passphrase()).toBeDisplayed();
  },
};

export const profileBlocked = {
  buy: () => tid('profile-blocked-buy'),
  key: () => tid('profile-blocked-key'),
  activate: () => tid('profile-blocked-activate'),
  fresh: () => tid('profile-blocked-fresh'),
};

export const asrModels = {
  download: (id: string) => tid(`asr-download-${id}`),
  cancel: (id: string) => tid(`asr-cancel-${id}`),
  enable: (id: string) => tid(`asr-enable-${id}`),
  enabled: (id: string) => tid(`asr-enabled-${id}`),
  menu: (id: string) => tid(`asr-menu-${id}`),
  delete: (id: string) => tid(`asr-delete-${id}`),
};
