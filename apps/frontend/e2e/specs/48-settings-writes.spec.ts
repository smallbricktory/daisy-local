// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// A setting written from one place survives a later write from another: every
// write carries only the fields it changes. The stored values are read from
// the world, so these run in browser mode.
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { tid } from '../pages/_shared.ts';
import { display, recordings, settings } from '../pages/settings.ts';

/** The webview zoom command `uiZoom.applyZoom` drives. */
const ZOOM_CMD = 'plugin:webview|set_webview_zoom';

type Stored = { machine: { settings: { ui_zoom: number } }; settings: { nav_order: string[] } };
const stored = () => browser.execute(() =>
  (window as unknown as { __e2e_world: { data: Stored } }).__e2e_world.data);

/** Drags one nav-rail item onto another, as the rail's HTML5 handlers read it.
 *  The drop is a separate turn: the rail re-renders with the dragged key first. */
async function dragNav(from: string, onto: string) {
  const fire = (type: string, k: string) => browser.execute((t: string, key: string) => {
    const w = window as unknown as { __e2e_dt?: DataTransfer };
    w.__e2e_dt ??= new DataTransfer();
    document.querySelector(`[data-testid="nav-${key}"]`)!
      .dispatchEvent(new DragEvent(t, { bubbles: true, cancelable: true, dataTransfer: w.__e2e_dt }));
  }, type, k);
  await fire('dragstart', from);
  await fire('dragover', onto);
  await fire('drop', onto);
  await fire('dragend', from);
}

describe('settings writes carry only what they change', () => {
  before(async function () {
    if (isNative()) return this.skip();
    await bootWorld();
  });

  it('a zoom set while Settings is open survives a later machine setting', async () => {
    await mockInvoke(ZOOM_CMD, null);
    try {
      await settings.open('display');
      await display.zoomInOnce();
      await browser.waitUntil(async () => (await stored()).machine.settings.ui_zoom !== 1, {
        timeoutMsg: 'the zoom was never stored',
      });
      const zoomed = (await stored()).machine.settings.ui_zoom;
      await settings.open('recordings');
      await recordings.flipToggle('reduce-background-noise');
      await recordings.flipToggle('reduce-background-noise');
      await browser.pause(200);
      expect((await stored()).machine.settings.ui_zoom).toBe(zoomed);
    } finally {
      await settings.open('display');
      await display.ensureZoomReset();
      await restoreInvoke(ZOOM_CMD);
    }
  });

  it('a nav order dragged while Settings is open survives a later profile setting', async () => {
    await settings.open('about');
    await dragNav('search', 'record');
    await browser.waitUntil(async () => (await stored()).settings.nav_order[0] === 'search', {
      timeoutMsg: 'the dragged order was never stored',
    });
    const order = (await stored()).settings.nav_order;
    const toggle = tid('toggle-check-for-new-versions-automatically');
    const before = await toggle.getAttribute('aria-checked');
    await toggle.click();
    await expect(toggle).toHaveAttribute('aria-checked', before === 'true' ? 'false' : 'true');
    await browser.pause(200);
    expect((await stored()).settings.nav_order).toEqual(order);
  });
});
