// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { recordings, settings } from '../pages/settings.ts';

const MEETING_SUPPORT = 'meeting_detection_support';
const MACHINE_SETTINGS = 'machine_settings';
const UPDATE_MACHINE_SETTINGS = 'update_machine_settings';

/** The reason a machine below macOS 14 gives for having no answer. */
const NO_SUPPORT = 'needs macOS 14';

describe('meeting prompt setting', () => {
  it('the switch is on by default and turns off', async () => {
    await bootWorld();
    await settings.open('recordings');
    await expect(recordings.toggle('meeting-prompt')).toHaveAttribute('aria-checked', 'true');
    await recordings.toggle('meeting-prompt').click();
    await expect(recordings.toggle('meeting-prompt')).toHaveAttribute('aria-checked', 'false');
  });

  it('a machine that cannot answer disables the switch and names the reason', async () => {
    // The browser world's fixture override has no reach into the real
    // backend, so native pins the same case through the command it calls.
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
    const machine = {
      machine: 'e2e-machine',
      settings: {
        default_mic_key: 'usb-mic', default_mic_source_id: 1, aec_mode_override: 'auto' as const,
        denoise_enabled: false, debug_level: 'off' as const, diarizer: 'speakrs', ui_zoom: 1,
        css_theme: null, appearance: 'system', mcp_enabled: false, mcp_port: 8765,
        mcp_allow_write: false, nerd_mode: true,
        show_live_captions: true,
        asr: { active: 'parakeet-tdt-0.6b-v2-int8', faint_audio_marker: true },
        meeting_prompt: { enabled: true, snooze_until: Math.floor(Date.now() / 1000) + 3600 },
      },
    };
    // The browser world's fixture override has no reach into the real
    // backend; native pins the snooze through the commands the page calls,
    // and mocks the write too so this profile's real machine settings never
    // pick up the fixture's fictional mic and diarizer.
    if (isNative()) {
      await mockInvoke(MACHINE_SETTINGS, machine);
      await mockInvoke(UPDATE_MACHINE_SETTINGS, machine);
    }
    try {
      await bootWorld({ machine });
      await settings.open('recordings');
      await expect(recordings.meetingPromptHelp()).toHaveText(expect.stringContaining('Paused until'));
      await recordings.meetingPromptAskAgain().click();
      await expect(recordings.meetingPromptHelp()).not.toBeExisting();
    } finally {
      if (isNative()) {
        await restoreInvoke(MACHINE_SETTINGS);
        await restoreInvoke(UPDATE_MACHINE_SETTINGS);
      }
    }
  });
});
