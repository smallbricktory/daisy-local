// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, mockInvoke, mockInvokeReject, restoreInvoke } from '../fixtures/invoke.ts';
import { about, calendars, mcp, settings, voiceprints } from '../pages/settings.ts';
import { confirmDialog, nav } from '../pages/shell.ts';
import { clickScrim, expectPicked } from '../pages/_shared.ts';

/** The seeded browser-world voiceprint. The native profile enrolls none. */
const SEEDED_VOICEPRINT = 'v-1';
const A_RENAMED_SPEAKER = 'Jordan R.';
const A_SPEAKER_EMAIL = 'jordan.r@example.com';

const A_CALENDAR = 'E2E calendar';

/** A `.test` URL (reserved TLD). Adding a subscription stores the URL; the
 *  fetch happens on Refresh, which this spec mocks. */
const AN_ICS_URL = 'https://calendar.example.test/e2e.ics';

/** Phrases that separate the two legal documents. */
const TOS_MARKER = 'These Terms of Service';
const PRIVACY_MARKER = 'local-first app';

describe('settings: voiceprints, calendars, mcp, about', () => {
  before(async () => {
    await bootWorld();
  });

  it('voiceprints: the tuning controls are live', async () => {
    await settings.open('voiceprints');
    await voiceprints.openAdvanced();
    await expect(voiceprints.tolerance()).toBeDisplayed();
  });

  it('voiceprints: an enrolled speaker is renamed through the modal', async function () {
    await settings.open('voiceprints');
    if ((await voiceprints.rename(SEEDED_VOICEPRINT).isExisting()) === false) {
      console.log('[e2e] this profile has no enrolled voiceprints — the table and its modal are not rendered; case skipped');
      return this.skip();
    }
    try {
      // The list is offered only once something is enrolled, and so is Rematch.
      await expect(voiceprints.rematch()).toBeEnabled();
      await voiceprints.renameTo(SEEDED_VOICEPRINT, A_RENAMED_SPEAKER, A_SPEAKER_EMAIL);
      await voiceprints.expectRowText(SEEDED_VOICEPRINT, A_RENAMED_SPEAKER, A_SPEAKER_EMAIL);
      await expect(voiceprints.menu(SEEDED_VOICEPRINT)).toBeDisplayed();
    } finally {
      await voiceprints.ensureModalClosed();
    }
  });

  it('voiceprints: the rename modal can be abandoned', async function () {
    await settings.open('voiceprints');
    if ((await voiceprints.rename(SEEDED_VOICEPRINT).isExisting()) === false) {
      console.log('[e2e] this profile has no enrolled voiceprints — the table and its modal are not rendered; case skipped');
      return this.skip();
    }
    const listed = await voiceprints.idByName(A_RENAMED_SPEAKER);
    try {
      await voiceprints.rename(SEEDED_VOICEPRINT).click();
      await expect(voiceprints.modal()).toBeDisplayed();
      await voiceprints.name().setValue('Discarded name');
      await voiceprints.cancel().click();
      await expect(voiceprints.modal()).not.toBeExisting();
      // And the page behind it dismisses it the same way.
      await voiceprints.rename(SEEDED_VOICEPRINT).click();
      await expect(voiceprints.modal()).toBeDisplayed();
      await clickScrim('vp-rename-backdrop');
      await expect(voiceprints.modal()).not.toBeExisting();
      await expect(await voiceprints.idByName('Discarded name')).toBe(null);
      await expect(await voiceprints.idByName(A_RENAMED_SPEAKER)).toBe(listed);
    } finally {
      await voiceprints.ensureModalClosed();
    }
  });

  it('calendars: a subscription is added, disabled, re-enabled and deleted', async () => {
    await settings.open('calendars');
    try {
      const id = await calendars.addSubscription(A_CALENDAR, AN_ICS_URL);
      // The form empties for the next one.
      await expect(calendars.name()).toHaveValue('');
      await expect(calendars.url()).toHaveValue('');
      await expect(calendars.color(id)).toBeDisplayed();
      await expectPicked(calendars.tag(id), '');

      await calendars.setEnabled(id, false);
      await calendars.setEnabled(id, true);

      await calendars.deleteSubscription(id);
      await expect(calendars.menu(id)).not.toBeExisting();
    } finally {
      await calendars.ensureAbsent(A_CALENDAR);
    }
    await expect(await calendars.idByName(A_CALENDAR)).toBe(null);
  });

  it('calendars: Refresh now runs one fetch and re-arms', async () => {
    await settings.open('calendars');
    // The command is mocked in both modes.
    await mockInvoke('refresh_calendars', { subscriptions_scanned: 1, events_loaded: 0, errors: [] });
    try {
      const id = await calendars.addSubscription(A_CALENDAR, AN_ICS_URL);
      await expect(calendars.refresh()).toBeEnabled();
      const before = (await callsOf('refresh_calendars')).length;
      await calendars.refresh().click();
      await expect(calendars.refresh()).toBeEnabled();
      await expect((await callsOf('refresh_calendars')).length).toBe(before + 1);
      await expect(calendars.menu(id)).toBeDisplayed();
    } finally {
      await calendars.ensureAbsent(A_CALENDAR);
      await restoreInvoke('refresh_calendars');
    }
  });

  it('mcp: enabling reveals the connect controls; disabling hides them', async () => {
    await settings.open('mcp');
    // mcp_apply is mocked in both modes.
    await mockInvoke('mcp_apply', {
      enabled: true, running: false, port: 32479, token: 'e2e-token',
      endpoint: 'http://127.0.0.1:32479/mcp',
      claude_command: 'claude mcp add daisy http://127.0.0.1:32479/mcp',
    });
    try {
      await expect(mcp.port()).toBeDisplayed();
      await expect(mcp.regenerate()).not.toBeExisting();

      await mcp.setEnabled(true);
      await expect(mcp.copy()).toBeDisplayed();
      // Copy and Regenerate are checked, not clicked.
      await expect(mcp.regenerate()).toBeEnabled();

      await mcp.flipAllowWrite();
      await mcp.flipAllowWrite();

      await mcp.setEnabled(false);
      await expect(mcp.copy()).not.toBeExisting();
    } finally {
      await mcp.ensureDisabled();
      await restoreInvoke('mcp_apply');
    }
  });

  it('about: the version and build id are shown', async () => {
    await settings.open('about');
    await expect(about.version()).toHaveText(expect.stringContaining('Version'));
    await expect(about.version()).toHaveText(expect.stringContaining('build'));
    // Behind the footer disclosure. The links are checked, not clicked.
    await about.openLegalCredits();
    await expect(about.linkSite()).toBeDisplayed();
    await expect(about.linkGithub()).toBeDisplayed();
  });

  it('about: the legal viewer shows both documents and closes', async () => {
    await settings.open('about');
    try {
      await about.openLegal();
      await about.chooseLegalTab('privacy', PRIVACY_MARKER);
      await about.chooseLegalTab('tos', TOS_MARKER);
      await about.legalClose().click();
      await expect(about.legalModal()).not.toBeExisting();
      // Clicking the page behind it closes it too.
      await about.openLegal();
      await clickScrim('about-legal-backdrop');
      await expect(about.legalModal()).not.toBeExisting();
    } finally {
      await about.ensureLegalClosed();
    }
  });

  it('about: the open-source licenses panel expands and collapses', async () => {
    await settings.open('about');
    try {
      await about.openLicenses();
      // The mirror link is checked, not clicked.
      await expect(about.ossMirror()).toBeDisplayed();
      await expect(about.modelLicenses()).toHaveText(expect.stringContaining('Gemma Terms of Use'));
      await expect(about.modelLicenseLinks()).toBeElementsArrayOfSize({ gte: 1 });
    } finally {
      await about.closeLicenses();
    }
  });

  it('about: the license card offers what this install\'s state allows', async () => {
    await settings.open('about');
    // The controls are checked, not clicked.
    if (await about.deactivate().isExisting()) {
      await expect(about.deactivate()).toBeEnabled();
      await expect(about.licenseKey()).not.toBeExisting();
      await expect(about.licensePlan()).toBeDisplayed();
      // Perpetual shows no date and offers no upgrade.
      if (await about.licensePerpetual().isExisting()) {
        await expect(about.licenseValidThrough()).not.toBeExisting();
        await expect(about.buyPerpetual()).not.toBeExisting();
      } else if (await about.buyPerpetual().isExisting()) {
        await expect(about.buyPerpetual()).toBeEnabled();
      }
      return;
    }
    await expect(about.licenseReplaced()).not.toBeExisting();
    await expect(about.licenseKey()).toBeDisplayed();
    await expect(about.getLicense()).toBeDisplayed();
    // Activate arms only on a pasted key.
    await expect(about.activate()).toBeDisabled();
  });

  it('about: the update check runs once and re-arms', async () => {
    await settings.open('about');
    // The command is mocked in both modes.
    await mockInvoke('check_for_update', {
      current: '9.9.9', latest: '9.9.9', update_available: false, notes: '', url: null,
    });
    try {
      const before = (await callsOf('check_for_update')).length;
      await about.checkUpdate().click();
      // The result lands in the shared alert dialog.
      await expect(confirmDialog.modal()).toBeDisplayed();
      await confirmDialog.acceptTyping();
      await expect(about.checkUpdate()).toBeEnabled();
      await expect((await callsOf('check_for_update')).length).toBe(before + 1);
    } finally {
      if (await confirmDialog.modal().isExisting()) await confirmDialog.acceptTyping();
      await restoreInvoke('check_for_update');
    }
  });

  it('opens with the error shown when the audio source listing fails', async () => {
    await mockInvokeReject('list_audio_sources', 'list sources: failed to connect: Host is down');
    try {
      // Settings loads on mount; leaving and returning remounts it.
      await nav.go('library');
      await settings.open('about');
      await expect(settings.error()).toHaveText(expect.stringContaining('Host is down'));
    } finally {
      await restoreInvoke('list_audio_sources');
      await nav.go('library');
    }
  });
});
