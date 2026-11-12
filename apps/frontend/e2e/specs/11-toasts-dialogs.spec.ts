// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { bootWorld } from '../fixtures/install.ts';
import { nav, toast, findBar } from '../pages/shell.ts';

const QUIET = 'audio-mic-very-quiet';
const MUTED = 'audio-mic-muted';
const SLEPT = 'recording-slept';

describe('toasts and the find bar', () => {
  before(async () => {
    await bootWorld();
    await nav.go('library');
  });

  it('an audio warning raises a toast; its X keeps that warning down', async () => {
    await emitAppEvent('audio-warning', { kind: 'mic-very-quiet' });
    await expect(toast.byId(QUIET)).toBeDisplayed();

    await toast.dismissById(QUIET);

    // Re-firing the identical warning must not bring it back. A different
    // warning right behind it proves the listener processed both.
    await emitAppEvent('audio-warning', { kind: 'mic-very-quiet' });
    await emitAppEvent('audio-warning', { kind: 'mic-muted-externally' });
    await expect(toast.byId(MUTED)).toBeDisplayed();
    await expect(toast.byId(QUIET)).not.toBeExisting();

    await toast.dismissById(MUTED);
  });

  it('a recording paused by a system sleep says so on wake', async () => {
    const slept = { session_id: 's-live', paused_at_unix_seconds: 1_790_888_552, resumed_at_unix_seconds: 1_790_888_764 };
    await emitAppEvent('recording:slept', { ...slept, resumed: true });
    await expect(toast.byId(SLEPT)).toHaveText(expect.stringContaining('Recording paused while the computer slept'));
    await expect(toast.byId(SLEPT)).toHaveText(expect.stringContaining('resumed'));
    await emitAppEvent('recording:slept', { ...slept, resumed: false });
    await expect(toast.byId(SLEPT)).toHaveText(expect.stringContaining('Press Resume'));
    await toast.dismissById(SLEPT);
  });

  it('a phone recording on its way in is visible before the library entry exists', async () => {
    const session = 'daisy-1700000001';
    const device = 'a1';
    // One toast per device carries everything that device is sending.
    const id = `sync-import-${device}`;
    const ev = {
      device_id: device, device_name: "Alex's phone", session,
      stage: 'receiving', received: 2, total: 8, title: null,
    };
    await emitAppEvent('sync:import', ev);
    await expect(toast.byId(id)).toBeDisplayed();
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('2 of 8'));

    await emitAppEvent('sync:import', { ...ev, stage: 'summary', received: null, total: null });
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('Writing the summary'));

    // One toast the whole way through, replaced in place, and the last stage
    // names the meeting that landed.
    await emitAppEvent('sync:import', {
      ...ev, stage: 'done', received: null, total: null, title: 'Harbour budget',
    });
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('Harbour budget'));
    await toast.dismissById(id);
  });

  it('a phone sending several recordings counts them in one toast', async () => {
    const device = 'b2';
    const id = `sync-import-${device}`;
    const ev = {
      device_id: device, device_name: "Alex's tablet", session: 'daisy-1700000100',
      stage: 'receiving', received: null, total: null, title: null,
    };
    for (const session of ['daisy-1700000100', 'daisy-1700000101', 'daisy-1700000102']) {
      await emitAppEvent('sync:import', { ...ev, session });
    }
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('3 recordings'));

    await emitAppEvent('sync:import', {
      ...ev, session: 'daisy-1700000100', stage: 'done', title: 'First',
    });
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('1 of 3 done'));

    for (const session of ['daisy-1700000101', 'daisy-1700000102']) {
      await emitAppEvent('sync:import', { ...ev, session, stage: 'done', title: 'Another' });
    }
    await expect(toast.byId(id)).toHaveText(expect.stringContaining('3 recordings arrived'));
    await toast.dismissById(id);
  });

  it('Ctrl/Cmd+F opens the find bar and its X closes it', async () => {
    await findBar.open();
    await findBar.typeQuery('meeting');
    await findBar.closeIt();
  });
});
