// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { ensureWorld } from '../fixtures/install.ts';
import { tid } from '../pages/_shared.ts';

describe('smoke', () => {
  before(async () => {
    // Native mode: the sandboxed config dir has no recorded EULA acceptance.
    await ensureWorld({ phase: { eula: false, consent: false, vaultExists: false, unlocked: false } });
  });

  it('boots to the EULA gate', async () => {
    await expect(tid('eula-accept')).toBeDisplayed();
  });

  it('round-trips a command over the plugin bridge (native only)', async function () {
    if (!isNative()) return this.skip();
    const accepted = await browser.tauri.execute(({ core }) => core.invoke('eula_status'));
    expect(accepted).toBe(false);
  });
});
