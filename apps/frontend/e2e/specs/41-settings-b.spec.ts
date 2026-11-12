// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { typeIntoEl } from '../pages/_shared.ts';
import { integrations, prompts, providers, recordings, settings, tags } from '../pages/settings.ts';

/** The built-in prompt the Summarizer runs; `crates/summarize/src/prompts.rs`
 *  mints the id and sorts it first. */
const DAISY_PROMPT = 'builtin:daisy';

/** The provider row Settings renders the key editor for — the one chosen under
 *  Recordings → AI features. */
const AI_PROVIDER = 'anthropic';

const A_TAG = 'e2e-alpha';
const A_RENAMED_TAG = 'e2e-beta';
const A_PROMPT = 'E2E digest';
const A_DESTINATION = 'E2E hook';
const AN_ABANDONED_DESTINATION = 'Abandoned hook';

/** `.test` is a reserved TLD (RFC 6761). */
const A_WEBHOOK_URL = 'https://hooks.example.test/e2e';

const A_TOKEN = 'token-e2e';

/** Not a credential. Typed only to arm the Test button; the spec clicks Test
 *  where the world answers the model list. */
const A_FAKE_KEY = 'sk-e2e-not-a-real-key';

describe('settings: tags, prompts, providers, integrations', () => {
  before(async () => {
    // No provider key: the key editor's Test button arms on what is typed,
    // which is the state the key-editor case reads. LM Studio is stored with
    // a model so the Recordings select offers it to the context-length case.
    await bootWorld({
      providers: [{ name: 'lm_studio', has_key: false, model: 'model-a', base_url: null, context_tokens: null }],
    });
  });

  it('tags: a swatch fills the hex field of a new tag', async () => {
    await settings.open('tags');
    await tags.fresh().click();
    await tags.swatch('7B5AD8').click();
    await expect(tags.colorHex()).toHaveValue('#7B5AD8');
    await tags.cancel().click();
  });

  it('tags: a tag is created, renamed and deleted', async () => {
    await settings.open('tags');
    try {
      const id = await tags.create(A_TAG, 'Lead with decisions.');
      await tags.select(id);
      await expect(tags.name()).toHaveValue(A_TAG);
      await expect(tags.prompt()).toHaveValue('Lead with decisions.');
      await tags.rename(id, A_RENAMED_TAG);
      await expect(await tags.idByName(A_TAG)).toBe(null);
    } finally {
      await tags.ensureDeleted(A_RENAMED_TAG);
      await tags.ensureDeleted(A_TAG);
    }
    await expect(await tags.idByName(A_RENAMED_TAG)).toBe(null);
  });

  it('tags: Cancel puts back what the selected tag stores', async () => {
    await settings.open('tags');
    try {
      const id = await tags.create(A_TAG);
      await tags.select(id);
      await tags.vocab().setValue('Northwind Logistics, NWL');
      await tags.cancel().click();
      await expect(tags.vocab()).toHaveValue('');
    } finally {
      await tags.ensureDeleted(A_TAG);
    }
  });

  it('prompts: a built-in is read-only until Save as…, and the copy can be the default', async () => {
    await settings.open('prompts');
    try {
      await prompts.choose(DAISY_PROMPT);
      // Built-in: the name is fixed and there is nothing to delete. Save is
      // disarmed because the draft still matches what is stored.
      await expect(prompts.name()).toBeDisabled();
      await expect(prompts.del()).not.toBeExisting();
      await expect(prompts.save()).toBeDisabled();
      await expect(prompts.reset()).toBeDisplayed();

      await prompts.saveAsNew(A_PROMPT);
      await expect(prompts.name()).toBeEnabled();
      await prompts.setDirective('List the three headlines.');

      await prompts.makeDefault().click();
      // The button is offered only by a prompt that is not the default yet.
      await expect(prompts.makeDefault()).not.toBeExisting();
    } finally {
      // Deleting the default hands the setting back to the built-in.
      await prompts.ensureDeleted(A_PROMPT);
    }
    await expect(await prompts.idByName(A_PROMPT)).toBe(null);
  });

  it('prompts: Save as… can be abandoned', async () => {
    await settings.open('prompts');
    await prompts.choose(DAISY_PROMPT);
    await prompts.saveAs().click();
    await expect(prompts.newName()).toBeDisplayed();
    await prompts.newCancel().click();
    await expect(prompts.newName()).not.toBeExisting();
    await expect(await prompts.idByName(A_PROMPT)).toBe(null);
  });

  it('providers: the key editor arms Test on a typed key and cancels back', async function () {
    await settings.open('providers');
    if (!(await providers.configure(AI_PROVIDER).isExisting())) {
      console.log('[e2e] no AI provider chosen in this profile; the key editor is not rendered — section skipped');
      return this.skip();
    }
    try {
      await providers.open(AI_PROVIDER);
      await expect(providers.key()).toBeDisplayed();
      await expect(providers.test()).toBeDisabled();
      await providers.key().setValue(A_FAKE_KEY);
      await expect(providers.test()).toBeEnabled();

      // Test asks the provider for its model list; step 2 needs the world to answer `list_provider_models`.
      if (!isNative()) {
        await providers.listModels();
        await expect(providers.modelCustom()).toBeDisplayed();
        await expect(providers.save()).toBeEnabled();
        await providers.back().click();
        await expect(providers.key()).toBeDisplayed();
        await providers.listModels();
        await providers.cancel2().click();
        await expect(providers.model()).not.toBeExisting();
      }
    } finally {
      await providers.ensureClosed(AI_PROVIDER);
    }
    await expect(providers.key()).not.toBeExisting();
  });

  it('providers: a local endpoint takes a context length, prefilled from LM Studio', async function () {
    // Step 2 needs the mocked model list and the mocked context probe.
    if (isNative()) return this.skip();
    // The Providers section edits the provider chosen under Recordings.
    await settings.open('recordings');
    await recordings.setAiProvider('lm_studio');
    await settings.open('providers');
    try {
      await providers.open('lm_studio');
      await expect(providers.key()).not.toBeExisting();
      await providers.listModels();
      // The world answers 32768 for LM Studio; a blank field takes it.
      await expect(providers.contextTokens()).toHaveValue('32768');
      await typeIntoEl(providers.contextTokens(), '65536');
      await providers.save().click();
      await expect(providers.configure('lm_studio')).toBeDisplayed();
      await expect(providers.rowMeta('lm_studio')).toHaveText(expect.stringContaining('ctx 65536'));

      // Reopening shows the stored value; clearing it removes it from the row.
      await providers.open('lm_studio');
      await providers.listModels();
      await expect(providers.contextTokens()).toHaveValue('65536');
      await providers.clearContextTokens();
      await providers.save().click();
      await expect(providers.configure('lm_studio')).toBeDisplayed();
      await expect(providers.rowMeta('lm_studio')).not.toHaveText(expect.stringContaining('ctx'));
    } finally {
      await providers.ensureClosed('lm_studio');
    }
  });

  it('integrations: a bearer webhook is added, disabled and deleted', async () => {
    await settings.open('integrations');
    try {
      const id = await integrations.createBearerWebhook(A_DESTINATION, A_WEBHOOK_URL, A_TOKEN);
      await integrations.expectRowText(id, A_WEBHOOK_URL, 'bearer');

      await integrations.openEditor(id);
      // Editing an existing destination defaults to keeping the stored token.
      await expect(integrations.auth('keep')).toBeSelected();
      await integrations.includeNotes().click();
      await integrations.includeTranscript().click();
      await integrations.includeTranscript().click();
      await integrations.enabled().click();
      await expect(integrations.enabled()).not.toBeSelected();
      await integrations.save().click();
      await expect(integrations.add()).toBeDisplayed();
      await integrations.expectRowText(id, 'summary, notes', 'disabled');
    } finally {
      await integrations.ensureAbsent(A_DESTINATION);
    }
    await expect(await integrations.idByName(A_DESTINATION)).toBe(null);
  });

  it('integrations: an abandoned destination is never stored', async () => {
    await settings.open('integrations');
    try {
      await integrations.add().click();
      await integrations.name().setValue(AN_ABANDONED_DESTINATION);
      await expect(integrations.save()).toBeDisabled();
      await integrations.url().setValue(A_WEBHOOK_URL);
      await expect(integrations.save()).toBeEnabled();
      await integrations.cancel().click();
      await expect(integrations.name()).not.toBeExisting();
    } finally {
      await integrations.ensureAbsent(AN_ABANDONED_DESTINATION);
    }
    await expect(await integrations.idByName(AN_ABANDONED_DESTINATION)).toBe(null);
  });
});
