// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { mockInvoke, restoreInvoke } from '../fixtures/invoke.ts';
import { workflows } from '../pages/workflows.ts';
import { expectPicked } from '../pages/_shared.ts';

/** Names this spec creates. Saving one writes the vault, so every case that
 *  does clears its own in a `finally`. */
const A_WORKFLOW = 'E2E after finalize';
const AN_EDITED_WORKFLOW = 'E2E edited rule';

/** The folder-picker command the write-to-folder step opens. */
const DIALOG_OPEN = 'pick_directory';
const A_FOLDER = '/Users/alex/Documents/daisy-exports';

describe('workflows', () => {
  before(async () => {
    await bootWorld();
    await workflows.open();
  });

  it('creates a workflow with one prompt step', async () => {
    try {
      const id = await workflows.create(A_WORKFLOW, 'finalized');
      await expect(workflows.edit(id)).toBeDisplayed();
      // The row summarizes the rule it saved.
      await expect(workflows.enabled(id)).toHaveAttribute('aria-checked', 'true');
      await expect(await workflows.idByName(A_WORKFLOW)).toBe(id);
    } finally {
      await workflows.ensureAbsent(A_WORKFLOW);
    }
    await expect(await workflows.idByName(A_WORKFLOW)).toBe(null);
  });

  it('edit adds a tag condition and saves; toggle disables; delete removes', async () => {
    try {
      const id = await workflows.create(AN_EDITED_WORKFLOW, 'finalized');
      await workflows.openEditor(id);
      await expect(workflows.name()).toHaveValue(AN_EDITED_WORKFLOW);
      // Add a tag condition with no tag.
      await workflows.addTagCondition('0', 0);
      // The tag combobox shows.
      await expect(workflows.condTagInput()).toBeDisplayed();
      await expectPicked(workflows.condMode('0'), 'all');
      await workflows.removeCondition('0-0');
      await workflows.saveEditor();

      await workflows.setEnabled(id, false);
      await workflows.deleteWorkflow(id);
    } finally {
      await workflows.ensureAbsent(AN_EDITED_WORKFLOW);
    }
    await expect(await workflows.idByName(AN_EDITED_WORKFLOW)).toBe(null);
  });

  it('cancel discards a new workflow', async () => {
    const before = await workflows.rowCount();
    try {
      await workflows.openNew();
      await workflows.typeName('E2E discarded');
      await workflows.cancelEditor();
    } finally {
      await workflows.ensureAbsent('E2E discarded');
    }
    await expect(workflows.rows()).toBeElementsArrayOfSize(before);
  });

  it('the condition tree offers all three kinds and a nested group', async () => {
    try {
      await workflows.openNew();
      await workflows.condAdd('0').click();
      await workflows.condAddTitle('0').click();
      await workflows.condTitle('0-0').setValue('design spec');
      await expect(workflows.condTitle('0-0')).toHaveValue('design spec');

      await workflows.condAdd('0').click();
      await workflows.condAddParticipant('0').click();
      await expect(workflows.condSelect('0-1')).toBeDisplayed();

      await workflows.condAddGroup('0').click();
      // The nested group runs the same editor one level down.
      await expectPicked(workflows.condMode('0-2'), 'any');
      await expect(workflows.condAddGroup('0-2')).toBeDisplayed();

      // A condition added through the nested group's own menu lands inside it,
      // not beside it at the root.
      await workflows.addTagCondition('0-2', 0);
      await expect(workflows.condRemove('0-3')).not.toBeExisting();
      await workflows.removeCondition('0-2-0');

      await workflows.removeCondition('0-2');
      await workflows.removeCondition('0-1');
      await workflows.removeCondition('0-0');
    } finally {
      if (await workflows.cancel().isExisting()) await workflows.cancelEditor();
    }
  });

  it('steps reorder, pick a folder and drop again; nothing is saved', async () => {
    // The folder picker is overridden in the page, so the step takes a path
    // without opening the OS dialog.
    await mockInvoke(DIALOG_OPEN, A_FOLDER);
    try {
      await workflows.openNew();
      await workflows.addPromptStep(0);
      await expect(workflows.stepUp(0)).toBeDisabled();

      await workflows.stepChooseDir(0).click();
      await expect(workflows.stepClearDir(0)).toBeDisplayed();
      await workflows.stepClearDir(0).click();
      await expect(workflows.stepChooseDir(0)).toBeDisplayed();

      await workflows.addIntegrationStep(1);
      await expect(workflows.stepUp(1)).toBeEnabled();
      await workflows.stepUp(1).click();
      // After the move the prompt step has index 1.
      await expect(workflows.stepPrompt(1)).toBeDisplayed();
      await workflows.stepDown(0).click();
      await expect(workflows.stepPrompt(0)).toBeDisplayed();

      await workflows.stepRemove(1).click();
      await expect(workflows.stepRemove(1)).not.toBeExisting();
    } finally {
      if (await workflows.cancel().isExisting()) await workflows.cancelEditor();
      await restoreInvoke(DIALOG_OPEN);
    }
  });
});
