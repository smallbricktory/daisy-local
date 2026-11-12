// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Settings › Recordings follows the background seal of recordings older
// builds left unencrypted.
import { expect } from '@wdio/globals';
import { emitAppEvent } from '../fixtures/events.ts';
import { bootWorld } from '../fixtures/install.ts';
import { recordings, settings } from '../pages/settings.ts';

describe('settings: recordings seal progress', () => {
  before(async () => {
    await bootWorld();
    await settings.open('recordings');
  });

  it('shows while recordings are left and goes once every one is sealed', async () => {
    await expect(recordings.sealProgress()).not.toBeExisting();
    await emitAppEvent('encryption:progress', { done: 2, total: 5 });
    await expect(recordings.sealProgress()).toHaveText(expect.stringContaining('2 of 5'));
    await emitAppEvent('encryption:progress', { done: 5, total: 5 });
    await expect(recordings.sealProgress()).not.toBeExisting();
  });
});
