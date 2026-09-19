// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { mockInvoke, mockInvokeReject, restoreInvoke } from '../fixtures/invoke.ts';
import { mobileSync, settings } from '../pages/settings.ts';
import { confirmDialog } from '../pages/shell.ts';

describe('settings: mobile sync', () => {
  before(async () => {
    await bootWorld();
    await settings.open('mobile sync');
  });

  it('shows the paired phones, or the empty line', async () => {
    if (isNative()) {
      await expect(mobileSync.empty()).toBeDisplayed();
      await expect(mobileSync.rows()).toBeElementsArrayOfSize(0);
      return;
    }
    await expect(mobileSync.rows()).toBeElementsArrayOfSize(1);
    await expect(mobileSync.row('phone-a1')).toHaveText(expect.stringContaining("Rowan's phone"));
  });

  it('says where a paired phone reaches this computer', async function () {
    if (isNative()) return this.skip();
    await expect(mobileSync.reach()).toHaveText(expect.stringContaining('studio-mac.local'));
    await expect(mobileSync.reach()).toHaveText(expect.stringContaining('7845'));
    await expect(mobileSync.reach()).toHaveText(expect.stringContaining('Bluetooth is on'));
  });

  it('names the recordings this computer cannot open and what makes them arrive', async function () {
    if (isNative()) return this.skip();
    await expect(mobileSync.refusedLine('phone-a1', 'sealed')).toHaveText(expect.stringContaining("One recording from Rowan's phone"));
    await expect(mobileSync.refusedLine('phone-a1', 'sealed')).toHaveText(expect.stringContaining('pair it again'));
  });

  it('show code renders a QR with a countdown; cancel removes it', async () => {
    await mobileSync.showCode().click();
    await expect(mobileSync.qr()).toBeDisplayed();
    await expect(mobileSync.countdown()).toHaveText(expect.stringMatching(/\d+ s/));
    await mobileSync.cancel().click();
    await expect(mobileSync.qr()).not.toBeExisting();
    await expect(mobileSync.showCode()).toBeDisplayed();
  });

  it('rename names the phone here and keeps the name it paired with beneath', async function () {
    if (isNative()) return this.skip();
    await mobileSync.renameDevice('phone-a1', 'Work iPhone');
    await expect(mobileSync.row('phone-a1')).toHaveText(expect.stringContaining('Work iPhone'));
    await expect(mobileSync.pairedAs('phone-a1')).toHaveText("Rowan's phone");
    await mobileSync.renameDevice('phone-a1', '');
    await expect(mobileSync.pairedAs('phone-a1')).not.toBeExisting();
    await expect(mobileSync.row('phone-a1')).toHaveText(expect.stringContaining("Rowan's phone"));
  });

  it('unpair asks, then drops the phone', async function () {
    if (isNative()) return this.skip();
    await mobileSync.unpairDevice('phone-a1');
    await confirmDialog.acceptTyping();
    await expect(mobileSync.rows()).toBeElementsArrayOfSize(0);
    await expect(mobileSync.empty()).toBeDisplayed();
    await expect(mobileSync.refused()).not.toBeExisting();
  });

  it('a phone that pairs while the screen is open appears without reopening it', async function () {
    if (isNative()) return this.skip();
    await expect(mobileSync.rows()).toBeElementsArrayOfSize(0);
    await mockInvoke('sync_status', {
      device_id: '7a1b2c3d4e5f60718293a4b5c6d7e8f9', machine_name: 'studio-mac',
      paired: [{
        device_id: 'phone-b2', name: "Wren's phone", phone_name: "Wren's phone", platform: 'ios',
        paired_at_unix: 1788900000, last_hello_unix: 1788900600,
      }],
      window: null, host: 'studio-mac.local', port: 7845,
      listening: true, bluetooth: 'on', refused: [], platform: 'macos',
    });

    await emitAppEvent('sync:changed', null);

    await expect(mobileSync.row('phone-b2')).toHaveText(expect.stringContaining("Wren's phone"));
    await expect(mobileSync.row('phone-b2')).toHaveText(expect.stringContaining('last seen'));
    await restoreInvoke('sync_status');
  });

  it('on Windows, says the firewall prompt is coming and what Cancel costs', async function () {
    if (isNative()) return this.skip();
    await expect(mobileSync.firewall()).not.toBeExisting();
    await mockInvoke('sync_status', {
      device_id: '7a1b2c3d4e5f60718293a4b5c6d7e8f9', machine_name: 'workshop-pc',
      paired: [], window: null, host: 'workshop-pc.local', port: 7845,
      listening: false, bluetooth: 'unsupported', refused: [], platform: 'windows',
    });
    await settings.open('recordings');
    await settings.open('mobile sync');
    await expect(mobileSync.firewall()).toHaveText(expect.stringContaining('Choose Allow'));
    await restoreInvoke('sync_status');
  });

  it('a failing status shows the error line', async () => {
    await mockInvokeReject('sync_status', 'vault locked');
    await settings.open('recordings');
    await settings.open('mobile sync');
    await expect(mobileSync.error()).toBeDisplayed();
    await restoreInvoke('sync_status');
  });
});
