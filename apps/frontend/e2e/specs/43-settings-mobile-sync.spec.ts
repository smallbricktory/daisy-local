// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { mockInvoke, mockInvokeReject, restoreInvoke } from '../fixtures/invoke.ts';
import { mobileSync, settings } from '../pages/settings.ts';
import { confirmDialog, toast } from '../pages/shell.ts';

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

  it('pair a device renders a QR with its quiet zone, the grouped key, and the warning; cancel removes it', async () => {
    await mobileSync.showCode().click();
    await expect(mobileSync.qr()).toBeDisplayed();
    await expect(mobileSync.qr()).toHaveAttribute('viewBox', expect.stringMatching(/^-4 -4 /));
    expect(await mobileSync.qrText()).toEqual(expect.stringMatching(/^daisy-pair:2:/));
    await expect(mobileSync.key()).toHaveText(expect.stringMatching(/^[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4}-[0-9A-Z]{4}$/));
    await expect(mobileSync.warning()).toHaveText('Anyone who can see this key while it is showing can pair a device.');
    await mobileSync.cancel().click();
    await expect(mobileSync.qr()).not.toBeExisting();
    await expect(mobileSync.showCode()).toBeDisplayed();
  });

  it('an expired key replaces the QR and key with a way to show a new one', async () => {
    await mockInvoke('sync_pairing_begin', {
      qr_text: 'daisy-pair:2:7K2MQX9D4HNBR8TC@studio-mac.local:7845',
      key_text: '7K2M-QX9D-4HNB-R8TC',
      expires_unix: Math.floor(Date.now() / 1000) - 1,
    });
    await mobileSync.showCode().click();
    await expect(mobileSync.qr()).not.toBeExisting();
    await expect(mobileSync.expired()).toHaveText('This key expired. Show a new one?');
    await restoreInvoke('sync_pairing_begin');
    await mobileSync.showNew().click();
    await expect(mobileSync.qr()).toBeDisplayed();
    await mobileSync.cancel().click();
    await expect(mobileSync.showCode()).toBeDisplayed();
  });

  it('leaving the screen ends a showing key', async () => {
    await mobileSync.showCode().click();
    await expect(mobileSync.qr()).toBeDisplayed();
    await settings.open('recordings');
    await settings.open('mobile sync');
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
        paired_at_unix: 1788900000, last_hello_unix: 1788900600, out_of_date: null,
      }],
      window: null, host: 'studio-mac.local', port: 7845,
      listening: true, bluetooth: 'on', refused: [], platform: 'macos',
    });

    await emitAppEvent('sync:changed', null);

    await expect(mobileSync.row('phone-b2')).toHaveText(expect.stringContaining("Wren's phone"));
    await expect(mobileSync.row('phone-b2')).toHaveText(expect.stringContaining('last seen'));
    await restoreInvoke('sync_status');
  });

  it('a device refused at the version gate says which side to update, on its row and in a toast', async function () {
    if (isNative()) return this.skip();
    const words = 'Your iOS device is out of date. Please update it to complete the sync. Sync has NOT been done.';
    await mockInvoke('sync_status', {
      device_id: '7a1b2c3d4e5f60718293a4b5c6d7e8f9', machine_name: 'studio-mac',
      paired: [{
        device_id: 'phone-b2', name: "Wren's phone", phone_name: "Wren's phone", platform: 'ios',
        paired_at_unix: 1788900000, last_hello_unix: 1788900600, out_of_date: 'phone',
      }],
      window: null, host: 'studio-mac.local', port: 7845,
      listening: true, bluetooth: 'on', refused: [], platform: 'macos',
    });

    await emitAppEvent('sync:gated', { device_id: 'phone-b2', platform: 'ios', out_of_date: 'phone' });
    await emitAppEvent('sync:changed', null);

    await expect(mobileSync.outOfDate('phone-b2')).toHaveText(words);
    await expect(mobileSync.row('phone-b2')).not.toHaveText(expect.stringContaining('last seen'));
    await expect(toast.byId('sync-gated-phone-b2')).toHaveText(expect.stringContaining(words));
    await toast.dismissById('sync-gated-phone-b2');
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

  it('on Linux with a firewall running, shows the command that opens the ports; with none, shows nothing', async function () {
    if (isNative()) return this.skip();
    await expect(mobileSync.linuxFirewall()).not.toBeExisting();
    const command = 'sudo ufw allow from 192.0.2.0/24 to any port 7845 proto tcp && sudo ufw allow from 192.0.2.0/24 to any port 5353 proto udp';
    await mockInvoke('linux_firewall_hint', { firewall: 'ufw', command });
    await settings.open('recordings');
    await settings.open('mobile sync');
    await expect(mobileSync.linuxFirewall()).toHaveText(
      expect.stringContaining('Daisy requires ports 7845 and 5353 to sync with local devices. Run the following command:'),
    );
    await expect(mobileSync.linuxFirewallCommand()).toHaveText(command);
    await expect(mobileSync.linuxFirewallCopy()).toBeDisplayed();
    await restoreInvoke('linux_firewall_hint');
    await settings.open('recordings');
    await settings.open('mobile sync');
    await expect(mobileSync.linuxFirewall()).not.toBeExisting();
  });

  it('with no network, showing a key says to connect and shows no key', async () => {
    const message = 'Connect this computer to Wi-Fi or a network to pair a phone.';
    await mockInvokeReject('sync_pairing_begin', { kind: 'config', code: 'DAISY-E014', message, friendly: message });
    await mobileSync.showCode().click();
    await expect(mobileSync.error()).toHaveText(expect.stringContaining(message));
    await expect(mobileSync.qr()).not.toBeExisting();
    await restoreInvoke('sync_pairing_begin');
  });

  it('with no network, the reach line says to connect', async function () {
    if (isNative()) return this.skip();
    await mockInvoke('sync_status', {
      device_id: 'd3a1c0de00000000000000000000beef',
      machine_name: 'studio-mac',
      paired: [{ device_id: 'phone-a1', name: "Rowan's phone", phone_name: "Rowan's phone", platform: 'ios', paired_at_unix: 1790000000, last_hello_unix: null, out_of_date: null }],
      window: null,
      host: null,
      port: 7845,
      listening: true,
      bluetooth: 'on',
      refused: [],
      platform: 'macos',
    });
    await settings.open('recordings');
    await settings.open('mobile sync');
    await expect(mobileSync.reach()).toHaveText(expect.stringContaining('Connect this computer to Wi-Fi or a network'));
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
