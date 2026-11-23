// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Browser mode only: an expired license and a foreign profile binding both
// need backend state the native sandbox can't be driven into.
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { ensureWorld } from '../fixtures/install.ts';
import { profileBlocked } from '../pages/gates.ts';
import { library } from '../pages/library.ts';
import { record } from '../pages/record.ts';
import { about, recordings, settings } from '../pages/settings.ts';
import { banner, nav } from '../pages/shell.ts';
import { expectPicked, typeIntoEl } from '../pages/_shared.ts';

const EXPIRED_TEXT = 'Your license has ended. Activate a license to record or import meetings.';

describe('license gates (browser mode)', function () {
  before(function () { if (isNative()) this.skip(); });

  it('an expired license opens straight to Library with a persistent banner, blocking record and import', async () => {
    await ensureWorld({ license: { state: 'expired' } });
    await expect(nav.item('library')).toBeDisplayed();
    await expect(banner.expired()).toBeDisplayed();
    await expect(banner.expired()).toHaveText(expect.stringContaining(EXPIRED_TEXT));
    await expect(library.newMeeting()).toBeDisabled();

    await nav.go('record');
    await expect(record.start()).toBeDisabled();

    const result: { ok: boolean; err?: unknown } = await browser.execute(async () => {
      const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args?: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__;
      try {
        await internals.invoke('start_recording', { req: { session_id: null, title: null } });
        return { ok: true };
      } catch (err) {
        return { ok: false, err };
      }
    });
    expect(result.ok).toBe(false);
    expect(JSON.stringify(result.err)).toContain('DAISY-E015');
  });

  it('activating a key from Settings → About clears the banner', async () => {
    await ensureWorld({ license: { state: 'expired' } });
    await expect(banner.expired()).toBeDisplayed();
    await banner.activate().click();
    await expect(settings.section('about')).toHaveElementClass('is-on');
    await expect(about.activate()).toBeDisabled();
    await typeIntoEl(about.licenseKey(), 'DAISY-TEST-KEY');
    await expect(about.activate()).toBeEnabled();
    await about.activate().click();
    await expect(banner.expired()).not.toBeExisting();
  });

  it('a profile bound to another install shows the blocked screen with a fresh-folder exit', async () => {
    await ensureWorld({ binding: { state: 'foreign' } });
    await expect(profileBlocked.fresh()).toBeDisplayed();
    await expect(profileBlocked.activate()).toBeDisplayed();
    await expect(profileBlocked.activate()).toBeDisabled();
    await typeIntoEl(profileBlocked.key(), 'DAISY-TEST-KEY');
    await expect(profileBlocked.activate()).toBeEnabled();
  });

  it('a refused Daisy Cloud registration says why and leaves the provider alone', async () => {
    await ensureWorld({
      gatewayError: 'Seat limit reached — deactivate Daisy on another device first.',
      providers: [
        { name: 'lm_studio', has_key: false, model: 'model-a', base_url: null, context_tokens: null },
        { name: 'daisy_gateway', has_key: false, model: null, base_url: null, context_tokens: null },
      ],
    });
    await settings.open('recordings');
    await recordings.setAiProvider('lm_studio');
    await recordings.tryAiProvider('daisy_gateway');
    await expect(recordings.aiProviderError()).toHaveText(expect.stringContaining('Seat limit reached'));
    await expectPicked(recordings.aiProvider(), 'lm_studio');
  });
});
