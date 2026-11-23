// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { $, browser, expect } from '@wdio/globals';
import { bootWorld } from '../fixtures/install.ts';
import { display, mobileSync, nerdMode, profile, recordings, settings } from '../pages/settings.ts';
import { nav } from '../pages/shell.ts';
import { isNative } from '../env.ts';
import { emitAppEvent } from '../fixtures/events.ts';
import { confirmDialog } from '../pages/shell.ts';
import { search } from '../pages/browse.ts';
import { aiRequired } from '../pages/shell.ts';
import { library } from '../pages/library.ts';
import { session, speakers } from '../pages/session.ts';
import type { Settings } from '../../src/tauri.ts';

describe('nerd mode', () => {
  before(async () => {
    await bootWorld();
  });

  afterEach(async () => {
    await nerdMode.set(true);
  });

  it('Display carries the switch, and flipping it writes', async () => {
    await settings.open('display');
    await expect(nerdMode.toggle()).toHaveAttribute('aria-checked', 'true');
    await nerdMode.set(false);
    await expect(nerdMode.toggle()).toHaveAttribute('aria-checked', 'false');
  });

  it('the tray icon switch shows only in Nerd Mode, and flipping it writes', async () => {
    await settings.open('display');
    const tray = $('[data-testid="toggle-show-tray-icon"]');
    await expect(tray).toHaveAttribute('aria-checked', 'true');
    await tray.click();
    await expect(tray).toHaveAttribute('aria-checked', 'false');
    await tray.click();
    await expect(tray).toHaveAttribute('aria-checked', 'true');
    await nerdMode.set(false);
    await expect(tray).not.toBeExisting();
  });

  it('the minimize-on-close switch shows only in Nerd Mode, and flipping it writes', async () => {
    await settings.open('display');
    const minimize = $('[data-testid="toggle-minimize-on-close"]');
    await expect(minimize).toHaveAttribute('aria-checked', 'false');
    await minimize.click();
    await expect(minimize).toHaveAttribute('aria-checked', 'true');
    await minimize.click();
    await expect(minimize).toHaveAttribute('aria-checked', 'false');
    await nerdMode.set(false);
    await expect(minimize).not.toBeExisting();
  });

  it('a close with no remembered choice asks, and the answer is remembered', async function () {
    if (isNative()) return this.skip();
    await settings.open('display');
    const minimize = $('[data-testid="toggle-minimize-on-close"]');
    await emitAppEvent('daisy://close/ask', null);
    await expect(confirmDialog.modal()).toBeDisplayed();
    await expect(confirmDialog.cancel()).not.toBeExisting();
    await confirmDialog.accept().click();
    await expect(confirmDialog.modal()).not.toBeExisting();
    await expect(minimize).toHaveAttribute('aria-checked', 'true');
    await emitAppEvent('daisy://close/ask', null);
    await confirmDialog.alternate().click();
    await expect(confirmDialog.modal()).not.toBeExisting();
    await expect(minimize).toHaveAttribute('aria-checked', 'false');
  });

  it('off hides Integrations, Voiceprints and MCP, and holds the pane on Recordings', async () => {
    await nerdMode.set(false);
    for (const s of ['integrations', 'voiceprints', 'mcp'] as const) {
      await expect(settings.section(s)).not.toBeExisting();
    }
    await expect(settings.section('recordings')).toBeExisting();
    await expect(settings.section('display')).toBeExisting();
    await expect(settings.section('providers')).toBeExisting();
    await nerdMode.set(true);
    await expect(settings.section('providers')).toBeExisting();
  });

  it('the header wears the badge only while Nerd Mode is on', async () => {
    await nerdMode.set(true);
    // Matches the badge text in either case.
    await browser.waitUntil(
      async () => /nerd mode/i.test((await nerdMode.badge().getText()).trim()),
      { timeoutMsg: 'the badge never showed its "Nerd Mode" text' },
    );
    await nerdMode.set(false);
    await expect(nerdMode.badge()).not.toBeExisting();
  });

  it('off hides Workflows, Analyzer and History from the rail', async () => {
    await nerdMode.set(false);
    expect(await nav.has('tasks')).toBe(false);
    expect(await nav.has('analyzer')).toBe(false);
    expect(await nav.has('history')).toBe(false);
    expect(await nav.has('library')).toBe(true);
    expect(await nav.has('search')).toBe(true);
    await nerdMode.set(true);
    expect(await nav.has('tasks')).toBe(true);
    expect(await nav.has('analyzer')).toBe(true);
    expect(await nav.has('history')).toBe(true);
  });

  it('off hides the advanced Recordings groups and the Vault group', async () => {
    await nerdMode.set(false);
    await settings.open('recordings');
    await expect(recordings.mic()).toBeDisplayed();
    await expect(recordings.toggle('show-captions-while-recording')).toBeDisplayed();
    await expect(recordings.speechLevelDevice()).not.toBeExisting();
    await expect(recordings.aec()).not.toBeExisting();
    await expect(recordings.aiProvider()).not.toBeExisting();
    await expect(recordings.debugLevel()).not.toBeExisting();

    await settings.open('profile');
    await expect(profile.openDir()).not.toBeExisting();
    await expect(profile.switchProfile()).not.toBeExisting();
    await expect(profile.move()).not.toBeExisting();
    await expect(profile.vaultChange()).not.toBeExisting();
    await expect(profile.vaultLock()).not.toBeExisting();

    await settings.open('display');
    await expect(display.cssTheme()).not.toBeExisting();

    await nerdMode.set(true);
    await settings.open('recordings');
    await expect(recordings.aec()).toBeDisplayed();
    await expect(recordings.debugLevel()).toBeDisplayed();
    await settings.open('profile');
    await expect(profile.vaultLock()).toBeDisplayed();
    await expect(profile.openDir()).toBeDisplayed();
    await settings.open('display');
    await expect(display.cssTheme()).toBeDisplayed();
  });

  const NO_PROVIDER = (): Settings => ({
    schema_version: 5, default_summary_provider: null, default_summary_prompt_id: null,
    user_display_name: 'Alex', nav_order: [], auto_update_check: false,
    reminder_lead_seconds: 60, voice_match_tolerance: 65,
  });

  it('off, the no-provider modal opens Providers directly', async function () {
    if (isNative()) return this.skip();
    await bootWorld({ providers: [], settings: NO_PROVIDER() });
    await nerdMode.set(false);
    await nav.go('search');
    await search.type('what was decided about the roadmap?');
    await expect(search.ask()).toBeEnabled();
    await search.ask().click();
    await expect(aiRequired.modal()).toBeDisplayed();
    await expect(aiRequired.openProviders()).toHaveText('Open Providers');
    await expect(aiRequired.nerdHint()).toHaveText(
      'To use a cloud provider, turn on Nerd Mode in Settings → Display.',
    );
    await aiRequired.openProviders().click();
    await expect(settings.section('providers')).toHaveElementClass('is-on');
  });

  it('flipping the switch from Display does not bounce off Display', async () => {
    await settings.open('about');
    await settings.open('display');
    await nerdMode.toggle().click();
    await expect(settings.section('display')).toHaveElementClass('is-on');
  });

  it('off, Participants is the people list plus Add', async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nerdMode.set(false);
    await nav.go('library');
    await library.openRow('s-001');
    await session.open('participants');
    await expect(speakers.addParticipant()).toBeDisplayed();
    await expect(speakers.recount()).not.toBeExisting();
    await expect(speakers.retrack()).not.toBeExisting();
    await expect(speakers.rediarize()).not.toBeExisting();
    await expect(speakers.row(0)).toHaveText(expect.stringContaining('jordan@example.com'));
    await expect(speakers.row(0)).not.toHaveText(expect.stringContaining('match 82%'));
    await expect(speakers.row(0)).not.toHaveText(expect.stringContaining('linked'));

    await nerdMode.set(true);
    await nav.go('library');
    await library.openRow('s-001');
    await session.open('participants');
    await expect(speakers.rediarize()).toBeDisplayed();
    await expect(speakers.row(0)).toHaveText(expect.stringContaining('match 82%'));
  });

  it('off, the session id and the repair items are gone', async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nerdMode.set(false);
    await nav.go('library');
    await library.openRow('s-001');
    await expect($('.summary-pane__sid')).not.toBeExisting();
    await session.openActions();
    await expect(session.action('export-md')).toBeDisplayed();
    await expect(session.action('delete-recording')).toBeDisplayed();
    await expect(session.action('polish')).not.toBeExisting();
    await expect(session.action('repair')).not.toBeExisting();
    await expect(session.action('split')).not.toBeExisting();
    await expect(session.action('delete-from')).not.toBeExisting();

    await nerdMode.set(true);
    await nav.go('library');
    await library.openRow('s-001');
    await expect($('.summary-pane__sid')).toBeDisplayed();
    await session.openActions();
    await expect(session.action('repair')).toBeDisplayed();
  });

  it('off, the machine chip and the reach line are gone', async function () {
    if (isNative()) return this.skip();
    await bootWorld();
    await nerdMode.set(false);
    await settings.open('recordings');
    await expect($('.set-group__scope')).not.toBeExisting();
    await settings.open('mobile sync');
    await expect(mobileSync.reach()).not.toBeExisting();

    await nerdMode.set(true);
    await settings.open('recordings');
    await expect($('.set-group__scope')).toBeDisplayed();
    await settings.open('mobile sync');
    await expect(mobileSync.reach()).toBeDisplayed();
  });
});
