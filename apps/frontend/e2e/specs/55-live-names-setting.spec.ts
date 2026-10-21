// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The Live speaker names choice (onboarding screen, Settings toggle) and the
// About page's release-notes link.
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld, ensureWorld } from '../fixtures/install.ts';
import { callsOf, mockInvokeWith, restoreInvoke } from '../fixtures/invoke.ts';
import { wizard } from '../pages/gates.ts';
import { about, recordings, settings } from '../pages/settings.ts';

const UPDATE_MACHINE_SETTINGS = 'update_machine_settings';
const OPEN_EXTERNAL = 'open_external';

/** Counts the machine-settings writes while the world still answers them. */
const countMachineWrites = () =>
  mockInvokeWith(UPDATE_MACHINE_SETTINGS, (args) =>
    (window as unknown as { __e2e_world: { handlers: Record<string, (a: unknown) => unknown> } })
      .__e2e_world.handlers.update_machine_settings(args));

const namesWrites = async () =>
  (await callsOf(UPDATE_MACHINE_SETTINGS)).map((w) => (w?.patch as Record<string, unknown>)?.live_speaker_names);

describe('live speaker names: onboarding', () => {
  before(async () => {
    await ensureWorld({ phase: { eula: true, consent: true, vaultExists: false, unlocked: false } });
  });

  it('the screen follows the microphone, starts on, and stores the choice made', async function () {
    if (isNative()) return this.skip();
    await wizard.toMicStep('Alex');
    await wizard.micContinue().click();
    await expect(wizard.namesContinue()).toBeDisplayed();
    await expect(wizard.namesOn()).toBeSelected();
    await expect(wizard.namesOff()).not.toBeSelected();

    await wizard.namesOff().click();
    await expect(wizard.namesOff()).toBeSelected();
    await countMachineWrites();
    try {
      await wizard.namesContinue().click();
      await expect(wizard.planRow('ai')).toBeDisplayed();
      expect(await namesWrites()).toContain(false);
    } finally {
      await restoreInvoke(UPDATE_MACHINE_SETTINGS);
    }
  });
});

describe('live speaker names: settings', () => {
  it('sits at the top of Recordings, is on by default, and saves a change', async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await settings.open('recordings');
    await expect(recordings.toggle('live-speaker-names')).toHaveAttribute('aria-checked', 'true');
    await countMachineWrites();
    try {
      await recordings.toggle('live-speaker-names').click();
      await expect(recordings.toggle('live-speaker-names')).toHaveAttribute('aria-checked', 'false');
      expect(await namesWrites()).toContain(false);
    } finally {
      await restoreInvoke(UPDATE_MACHINE_SETTINGS);
    }
  });
});

describe('release notes link', () => {
  it('opens the changelog at the running version in the external browser', async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await mockInvokeWith(OPEN_EXTERNAL, () => null);
    try {
      await settings.open('about');
      await expect(about.releaseNotes()).toBeDisplayed();
      await about.releaseNotes().click();
      const calls = await callsOf(OPEN_EXTERNAL);
      expect(calls).toEqual([{ url: 'https://www.daisylocal.app/changelog#9.9.9' }]);
    } finally {
      await restoreInvoke(OPEN_EXTERNAL);
    }
  });
});
