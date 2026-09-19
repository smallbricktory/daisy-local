// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { $, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { search } from '../pages/browse.ts';
import { clickRowMenuItem, expectPicked } from '../pages/_shared.ts';
import { aiRequired, confirmDialog, nav } from '../pages/shell.ts';
import { asrModels as asrRows } from '../pages/gates.ts';
import { asrModels, localModel, providers, recordings, settings } from '../pages/settings.ts';
import type { Settings } from '../../src/tauri.ts';

const ID = 'gemma-4-e4b-it-q4';
const NO_PROVIDER = (): Settings => ({
  schema_version: 5, default_summary_provider: null, default_summary_prompt_id: null,
  user_display_name: 'Alex', nav_order: [], auto_update_check: false,
  reminder_lead_seconds: 60, voice_match_tolerance: 65,
});

describe('settings: the built-in model', () => {
  before(async () => {
    // No provider at all: the download is what makes one. The rows read the
    // fixture world; a native run reaches the real download, so it checks the
    // license links alone.
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
    });
  });

  it('lists the row with the capability line and a Download button', async function () {
    if (isNative()) return this.skip();
    await settings.open('providers');
    await expect(localModel.row(ID)).toBeDisplayed();
    await expect(localModel.meta(ID)).toHaveText(expect.stringContaining('Fast on this machine'));
    await expect(localModel.download(ID)).toBeDisplayed();
  });

  it('names each model license as a link', async () => {
    await settings.open('providers');
    // The links open a URL in the system browser; they are read, never clicked.
    await expect(localModel.license(ID)).toHaveText('Gemma Terms of Use');
    await expect(asrModels.license('parakeet-tdt-0.6b-v2-int8')).toHaveText('CC-BY-4.0');
  });

  it('download installs it and it becomes the AI provider', async function () {
    if (isNative()) return this.skip();
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.menu(ID)).toBeDisplayed();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'local');
  });

  it('delete removes it and clears the provider', async function () {
    if (isNative()) return this.skip();
    await settings.open('providers');
    await clickRowMenuItem(`llm-menu-${ID}`, `llm-delete-${ID}`);
    await confirmDialog.acceptTyping();
    await expect(localModel.download(ID)).toBeDisplayed();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), '');
  });

  it('an unsupported machine shows the hardware line and no Download', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      llmModels: [{ id: ID, display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'unsupported', capability_line: 'Needs a graphics card with 6 GB of memory or more.', partial_bytes: 0 }],
    });
    await settings.open('providers');
    await expect(localModel.meta(ID)).toHaveText(expect.stringContaining('Needs a graphics card'));
    await expect(localModel.download(ID)).not.toBeExisting();
  });

  it('an unsupported machine cannot enable a model it already downloaded', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmModels: [{ id: ID, display: 'Gemma 4 E4B', installed: true, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'unsupported', capability_line: 'Needs a graphics card with 6 GB of memory or more.', partial_bytes: 0 }],
    });
    await settings.open('providers');
    await expect(localModel.meta(ID)).toHaveText(expect.stringContaining('Needs a graphics card'));
    await expect(localModel.enable(ID)).not.toBeExisting();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), '');
  });

  it('cancel during the download returns the row to idle without an error', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmDownloadHold: 'downloading',
    });
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.progress(ID)).toHaveText('42%');
    await localModel.cancel(ID).click();
    await expect(localModel.download(ID)).toHaveText('Download');
    await expect(localModel.menu(ID)).not.toBeExisting();
    await expect($('.meta--danger')).not.toBeExisting();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), '');
  });

  it('leaving Providers mid-download and coming back shows the same download', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmDownloadHold: 'downloading',
    });
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.progress(ID)).toHaveText('42%');
    // The download outlives the page that started it: Providers unmounts on
    // the way to the library and mounts again on the way back.
    await nav.go('library');
    await settings.open('providers');
    await expect(localModel.progress(ID)).toHaveText('42%');
    await expect(localModel.cancel(ID)).toBeDisplayed();
    await expect(localModel.download(ID)).not.toBeExisting();
    await localModel.cancel(ID).click();
    await expect(localModel.download(ID)).toHaveText('Download');
  });

  it('a wait between attempts names the attempt and is not painted as an error', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmDownloadHold: 'retrying',
    });
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.progress(ID)).toHaveText('Retrying 2 of 5');
    await expect($('*=Attempt 1 failed')).toBeDisplayed();
    await expect($('.meta--danger')).not.toBeExisting();
  });

  it('cancel during the warm-up leaves the model installed and selected', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmDownloadHold: 'warming',
    });
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.progress(ID)).toHaveText('Warming up…');
    await localModel.cancel(ID).click();
    await expect(localModel.menu(ID)).toBeDisplayed();
    await expect($('.meta--danger')).not.toBeExisting();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'local');
  });

  it('a stopped download offers to free the bytes it staged', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmModels: [{ id: ID, display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 3_200_000_000 }],
    });
    await settings.open('providers');
    await expect(localModel.partialMenu(ID)).toBeDisplayed();
    await clickRowMenuItem(`llm-partial-menu-${ID}`, `llm-remove-partial-${ID}`);
    await confirmDialog.acceptTyping();
    await expect(localModel.partialMenu(ID)).not.toBeExisting();
    await expect(localModel.download(ID)).toBeDisplayed();
  });

  it('the no-provider modal hands the download to Providers', async function () {
    if (isNative()) return this.skip();
    await bootWorld({ providers: [], settings: NO_PROVIDER() });
    await nav.go('search');
    await search.type('what was decided about the roadmap?');
    await expect(search.ask()).toBeEnabled();
    await search.ask().click();
    await expect(aiRequired.modal()).toBeDisplayed();
    await aiRequired.builtin().click();
    await expect(aiRequired.modal()).not.toBeExisting();
    // Providers opens with the download already running; it ends installed.
    await expect(localModel.menu(ID)).toBeDisplayed();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'local');
  });

  it('the no-provider modal selects a built-in model that is already installed', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmModels: [{ id: ID, display: 'Gemma 4 E4B', installed: true, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0 }],
    });
    await nav.go('search');
    await search.type('what was decided about the roadmap?');
    await search.ask().click();
    await expect(aiRequired.modal()).toBeDisplayed();
    await expect(aiRequired.builtin()).toHaveText("Use Daisy's built-in model");
    await aiRequired.builtin().click();
    await expect(aiRequired.modal()).not.toBeExisting();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'local');
  });

  it('Enable makes an installed built-in model the AI provider', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      llmModels: [{ id: ID, display: 'Gemma 4 E4B', installed: true, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0 }],
    });
    await settings.open('providers');
    await expect(providers.enabled('anthropic')).toBeDisplayed();
    await localModel.enable(ID).click();
    await expect(localModel.enabled(ID)).toBeDisplayed();
    await expect(providers.enable('anthropic')).toBeDisplayed();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'local');
  });

  it('a provider with nothing to configure is a row that enables like the others', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [
        { name: 'anthropic', has_key: true, model: 'model-a', base_url: null, context_tokens: null },
        { name: 'daisy_gateway', has_key: false, model: null, base_url: null, context_tokens: null },
      ],
    });
    await settings.open('providers');
    await expect(providers.row('daisy_gateway')).toHaveText(expect.stringContaining('(Internal use only)'));
    await providers.enable('daisy_gateway').click();
    await expect(providers.enabled('daisy_gateway')).toBeDisplayed();
    await expect(providers.enableError()).not.toBeExisting();
    await settings.open('recordings');
    await expectPicked(recordings.aiProvider(), 'daisy_gateway');
  });

  it('a downloaded transcription model enables', async function () {
    if (isNative()) return this.skip();
    const WHISPER = 'ggml-base.en.bin';
    await bootWorld({
      asrModels: [
        { id: 'parakeet-tdt-0.6b-v2-int8', engine: 'parakeet', display: 'Parakeet (English)', installed: true, active: true, bundled_floor: true, profile_copy: false, multilingual: false, bytes: 640_000_000, license: 'CC-BY-4.0', license_url: 'https://creativecommons.org/licenses/by/4.0/' },
        { id: WHISPER, engine: 'whisper', display: 'Whisper base (English)', installed: true, active: false, bundled_floor: false, profile_copy: true, multilingual: false, bytes: 148_000_000, license: 'MIT', license_url: 'https://github.com/openai/whisper/blob/main/LICENSE' },
      ],
    });
    await settings.open('providers');
    await expect(asrRows.enabled('parakeet-tdt-0.6b-v2-int8')).toBeDisplayed();
    await asrRows.enable(WHISPER).click();
    await expect(asrRows.enabled(WHISPER)).toBeDisplayed();
    await expect(asrRows.enable('parakeet-tdt-0.6b-v2-int8')).toBeDisplayed();
  });

  it('a failed warm-up leaves the row showing the model it installed', async function () {
    if (isNative()) return this.skip();
    const WARM_FAIL = 'warm-fails';
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmModels: [{ id: WARM_FAIL, display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0 }],
    });
    await settings.open('providers');
    await localModel.download(WARM_FAIL).click();
    await expect(localModel.menu(WARM_FAIL)).toBeDisplayed();
    await expect($('.meta--danger')).toHaveText(expect.stringContaining('would not start on this computer'));
  });

  it('a failed download offers Retry from the main-window toast', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmModels: [{ id: 'warm-fails', display: 'Gemma 4 E4B', installed: false, bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0 }],
    });
    await settings.open('providers');
    await localModel.download('warm-fails').click();
    await expect($('[data-testid="toast-action-retry"]')).toBeDisplayed();
  });

  it('a download in flight keeps the summary buttons off', async function () {
    if (isNative()) return this.skip();
    await bootWorld({
      providers: [],
      settings: NO_PROVIDER(),
      llmDownloadHold: 'downloading',
    });
    await settings.open('providers');
    await localModel.download(ID).click();
    await expect(localModel.progress(ID)).toHaveText('42%');
    await nav.go('search');
    await search.type('what was decided about the roadmap?');
    await search.ask().click();
    // Installing is not a missing provider: the toast is the surface, not the modal.
    await expect(aiRequired.modal()).not.toBeExisting();
  });
});
