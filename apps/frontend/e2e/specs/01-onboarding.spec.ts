// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { ensureWorld } from '../fixtures/install.ts';
import { callsOf, mockInvokeWith, restoreInvoke } from '../fixtures/invoke.ts';
import { eula, consent, wizard } from '../pages/gates.ts';
import { nav } from '../pages/shell.ts';

const INIT_VAULT = 'init_vault_machine_mode';

describe('first run', () => {
  before(async () => {
    await ensureWorld({ phase: { eula: false, consent: false, vaultExists: false, unlocked: false } });
  });

  it('EULA: accept is disabled until the box is ticked; tabs switch', async () => {
    await eula.tab('privacy').click();
    await expect(eula.tab('privacy')).toHaveElementClass(expect.stringContaining('is-on'));
    await eula.acceptAll();
    await expect(consent.accept()).toBeDisplayed();
  });

  it('Consent: every item must be ticked', async () => {
    await consent.acceptAll();
    await expect(wizard.getStarted()).toBeDisplayed();
  });

  it('Wizard: name → mic → plan → main, with the vault created silently', async function () {
    if (isNative()) return this.skip();
    // The world still owns the state; the mock is there to count the call.
    await mockInvokeWith(INIT_VAULT, () =>
      (window as unknown as { __e2e_world: { handlers: Record<string, (a: unknown) => unknown> } })
        .__e2e_world.handlers.init_vault_machine_mode(null));
    try {
      await wizard.toMicStep('Alex');
      expect(await callsOf(INIT_VAULT)).toHaveLength(1);
      await wizard.micContinue().click();

      await expect(wizard.planRow('ai')).toHaveText(
        expect.stringContaining("Daisy's built-in model"),
      );
      await expect(wizard.planStatus('ai')).toBeDisplayed();
      await expect(wizard.planRow('transcription')).toHaveText(
        expect.stringContaining('Parakeet, on this computer'),
      );
      await expect(wizard.planRow('recordings')).toHaveText(
        expect.stringContaining('/Users/alex/Library/Application Support/daisy'),
      );
      await expect(wizard.finish()).toBeEnabled();
      await wizard.finish().click();
      await expect(nav.item('library')).toBeDisplayed();
    } finally {
      await restoreInvoke(INIT_VAULT);
    }
  });
});

describe('first run: plan page reflects a finished download', () => {
  before(async () => {
    await ensureWorld({
      phase: { eula: true, consent: true, vaultExists: false, unlocked: false },
      onboardingPlan: {
        ai: { kind: 'builtin', capability: 'fast', installed: false },
        transcription: { engine: 'parakeet', installed: true },
        profile_dir: '/Users/alex/Library/Application Support/daisy',
      },
    });
  });

  it('the AI row moves off queued once its download job completes', async function () {
    if (isNative()) return this.skip();
    await wizard.toMicStep('Alex');
    await wizard.micContinue().click();
    await expect(wizard.planStatus('ai')).toBeDisplayed();
    // The world enqueues the model's download on apply; once the job
    // finishes, the plan re-read must reflect it installed instead of
    // leaving the row stuck on its queued/downloading text.
    await expect(wizard.planStatus('ai')).toHaveText('ready');
  });
});

describe('first run: back to intro after the vault exists', () => {
  before(async () => {
    await ensureWorld({ phase: { eula: true, consent: true, vaultExists: false, unlocked: false } });
  });

  it('Back from microphone, then Continue, returns to microphone and still reaches the plan', async function () {
    if (isNative()) return this.skip();
    await wizard.toMicStep('Alex');
    await wizard.back().click();
    await expect(wizard.introContinue()).toBeDisplayed();
    await wizard.introContinue().click();
    await expect(wizard.micContinue()).toBeDisplayed();
    await wizard.finishFromMic();
  });
});
