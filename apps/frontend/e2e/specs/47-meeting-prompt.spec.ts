// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { nerdMode, recordings, settings } from '../pages/settings.ts';

const MEETING_SUPPORT = 'meeting_detection_support';
const UPDATE_MACHINE_SETTINGS = 'update_machine_settings';

/** The reason a machine below macOS 14 gives for having no answer. */
const NO_SUPPORT = 'needs macOS 14';

type TauriGlobal = {
  __TAURI__?: { core?: { invoke: (cmd: string, args: unknown) => Promise<unknown> } };
};

/** Writes the fields `patch` carries into the real backend's machine
 *  settings, as the page's own writes do. */
async function patchMachine(patch: Record<string, unknown>): Promise<void> {
  await browser.execute(async (cmd: string, p: Record<string, unknown>) => {
    const core = (window as unknown as TauriGlobal).__TAURI__?.core;
    if (!core) throw new Error('window.__TAURI__.core missing — is this a ui-test build?');
    await core.invoke(cmd, { patch: p });
  }, UPDATE_MACHINE_SETTINGS, patch);
}

describe('meeting prompt setting', () => {
  it('the stop switch is on by default and turns off without touching the start switch', async () => {
    await bootWorld();
    await settings.open('recordings');
    await expect(recordings.meetingStopToggle()).toHaveAttribute('aria-checked', 'true');
    await recordings.meetingStopToggle().click();
    await expect(recordings.meetingStopToggle()).toHaveAttribute('aria-checked', 'false');
    await expect(recordings.toggle('meeting-prompt')).toHaveAttribute('aria-checked', 'true');
  });

  it('both meeting switches live behind Nerd Mode', async () => {
    await bootWorld();
    await nerdMode.set(false);
    await settings.open('recordings');
    await expect(recordings.toggle('meeting-prompt')).not.toBeExisting();
    await expect(recordings.meetingStopToggle()).not.toBeExisting();
    await nerdMode.set(true);
    await settings.open('recordings');
    await expect(recordings.toggle('meeting-prompt')).toBeDisplayed();
    await expect(recordings.meetingStopToggle()).toBeDisplayed();
  });

  it('the switch is on by default and turns off', async () => {
    await bootWorld();
    await settings.open('recordings');
    await expect(recordings.toggle('meeting-prompt')).toHaveAttribute('aria-checked', 'true');
    await recordings.toggle('meeting-prompt').click();
    await expect(recordings.toggle('meeting-prompt')).toHaveAttribute('aria-checked', 'false');
  });

  it('a machine that cannot answer disables the switch and names the reason', async () => {
    // Native mode pins the case through the command the page calls.
    if (isNative()) await mockInvoke(MEETING_SUPPORT, { supported: false, reason: NO_SUPPORT });
    try {
      await bootWorld({ meetingSupport: { supported: false, reason: NO_SUPPORT } });
      await settings.open('recordings');
      await expect(recordings.toggle('meeting-prompt')).toBeDisabled();
      await expect($(`.set-row__help*=${NO_SUPPORT}`)).toBeDisplayed();
    } finally {
      if (isNative()) await restoreInvoke(MEETING_SUPPORT);
    }
  });

  it('a snooze reads as a pause, and Ask again clears it', async () => {
    const snoozed = {
      enabled: true, snooze_until: Math.floor(Date.now() / 1000) + 3600, stop_enabled: true,
    };
    try {
      if (isNative()) {
        // The snooze is written to the real backend, one field, and cleared below.
        await bootWorld();
        await patchMachine({ meeting_prompt: snoozed });
      } else {
        const machine = {
          machine: 'e2e-machine',
          settings: {
            default_mic_key: 'usb-mic', default_mic_source_id: 1, aec_mode_override: 'auto' as const,
            denoise_enabled: false, debug_level: 'off' as const, ui_zoom: 1,
            css_theme: null, appearance: 'system', show_tray_icon: true, minimize_on_close: null, mcp_enabled: false, mcp_port: 8765,
            mcp_allow_write: false, nerd_mode: true,
            show_live_captions: true,
            live_speaker_names: true,
            asr: { active: 'nemotron-speech-streaming-en-0.6b-560ms-int8' },
            meeting_prompt: snoozed,
          },
        };
        await bootWorld({ machine });
      }
      await settings.open('recordings');
      await expect(recordings.meetingPromptHelp()).toHaveText(expect.stringContaining('Paused until'));
      await recordings.meetingPromptAskAgain().click();
      await expect(recordings.meetingPromptHelp()).not.toBeExisting();
    } finally {
      if (isNative()) {
        await patchMachine({ meeting_prompt: { enabled: true, snooze_until: null, stop_enabled: true } });
      }
    }
  });
});
