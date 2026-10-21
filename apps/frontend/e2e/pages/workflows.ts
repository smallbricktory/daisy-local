// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the Workflows route: the list rows, the editor form, the
// recursive condition tree and the action-step list.
import { expect } from '@wdio/globals';
import {
  clickRowMenuItem, expectPicked, idByText, selectOption, setCheckbox, tid, tids, typeInto,
  waitForIdByName,
} from './_shared.ts';
import { confirmDialog, nav } from './shell.ts';
import { ensureAbsent } from './teardown.ts';

/** Triggers the editor's When select offers, as the backend spells them. */
export type Trigger = 'finalized' | 'deleted' | 'imported' | 'finalize_failed';

export const workflows = {
  fresh: () => tid('wf-new'),
  /** The block an empty list paints, and the editor it opens. */
  empty: () => tid('wf-empty'),
  emptyNew: () => tid('wf-empty-new'),
  name: () => tid('wf-name'),
  trigger: () => tid('wf-trigger'),
  save: () => tid('wf-save'),
  cancel: () => tid('wf-cancel'),
  enabled: (id: string) => tid(`wf-enabled-${id}`),
  edit: (id: string) => tid(`wf-edit-${id}`),
  menu: (id: string) => tid(`wf-menu-${id}`),
  /** One Edit button per listed workflow, so its length is the list length. */
  rows: () => tids('wf-edit-'),

  // Condition tree. `path` is the node's index path: the root group is `0`,
  // its i-th child `0-i`. Every control belongs to the group whose path it
  // carries, the add-condition menu included.
  condAdd: (path: string) => tid(`cond-add-${path}`),
  condAddTag: (path: string) => tid(`cond-add-tag-${path}`),
  condAddParticipant: (path: string) => tid(`cond-add-participant-${path}`),
  condAddTitle: (path: string) => tid(`cond-add-title-${path}`),
  condMode: (path: string) => tid(`cond-mode-${path}`),
  condSelect: (path: string) => tid(`cond-select-${path}`),
  condTitle: (path: string) => tid(`cond-title-${path}`),
  condRemove: (path: string) => tid(`cond-remove-${path}`),
  condAddGroup: (path: string) => tid(`cond-add-group-${path}`),
  /** A fresh tag condition holds no tag, so it renders the shared tag
   *  combobox; the editor has no other one on screen. */
  condTagInput: () => tid('tag-combobox-input'),

  // Action steps, indexed by position in the list.
  stepAdd: () => tid('step-add'),
  stepAddPrompt: () => tid('step-add-prompt'),
  stepAddIntegration: () => tid('step-add-integration'),
  stepPrompt: (i: number) => tid(`step-prompt-${i}`),
  stepSendTo: (i: number) => tid(`step-send-to-${i}`),
  stepChooseDir: (i: number) => tid(`step-choose-dir-${i}`),
  stepClearDir: (i: number) => tid(`step-clear-dir-${i}`),
  stepIntegration: (i: number) => tid(`step-integration-${i}`),
  stepUp: (i: number) => tid(`step-up-${i}`),
  stepDown: (i: number) => tid(`step-down-${i}`),
  stepRemove: (i: number) => tid(`step-remove-${i}`),

  async rowCount(): Promise<number> {
    return (await this.rows()).length;
  },

  /** Id of the listed workflow carrying this name, or null. */
  async idByName(name: string): Promise<string | null> {
    return idByText('wf-edit-', name, 'parent');
  },

  /** Routes to Workflows and waits for the list. */
  async open() {
    await nav.go('tasks');
    await expect(this.fresh()).toBeDisplayed();
  },

  /** Opens the empty editor. */
  async openNew() {
    await this.fresh().click();
    await expect(this.name()).toBeDisplayed();
  },

  /** Types the workflow's name; the box keeps it. */
  async typeName(n: string) {
    await typeInto('wf-name', n);
    await expect(this.name()).toHaveValue(n);
  },

  /** Reopens one workflow's editor, seeded from it. */
  async openEditor(id: string) {
    await this.edit(id).click();
    await expect(this.name()).toBeDisplayed();
  },

  /** Picks the trigger; the select keeps it. */
  async chooseTrigger(t: Trigger) {
    await selectOption('wf-trigger', t);
    await expectPicked(this.trigger(), t);
  },

  /** Adds a run-prompt step at position `i`. The choice is offered only on
   *  the Recording-finalized trigger. */
  async addPromptStep(i: number) {
    await this.stepAdd().click();
    await this.stepAddPrompt().click();
    await expect(this.stepPrompt(i)).toBeDisplayed();
  },

  /** Adds a send-to-integration step at position `i`. */
  async addIntegrationStep(i: number) {
    await this.stepAdd().click();
    await this.stepAddIntegration().click();
    await expect(this.stepRemove(i)).toBeDisplayed();
  },

  /** Adds an empty tag condition to the group at `path`; `i` is the position
   *  it takes inside that group. */
  async addTagCondition(path: string, i: number) {
    await this.condAdd(path).click();
    await this.condAddTag(path).click();
    await expect(this.condRemove(`${path}-${i}`)).toBeDisplayed();
  },

  /** Drops one condition from its parent group. */
  async removeCondition(path: string) {
    await this.condRemove(path).click();
    await expect(this.condRemove(path)).not.toBeExisting();
  },

  /** Saves the editor; the list comes back. */
  async saveEditor() {
    await expect(this.save()).toBeEnabled();
    await this.save().click();
    await expect(this.fresh()).toBeDisplayed();
  },

  /** Abandons the editor; the list comes back with nothing written. */
  async cancelEditor() {
    await this.cancel().click();
    await expect(this.fresh()).toBeDisplayed();
  },

  /** Fills the editor and saves; the workflow joins the list and its id comes
   *  back. Saving writes the vault, so a spec pairs this with `ensureAbsent`. */
  async create(name: string, trigger: Trigger): Promise<string> {
    await this.openNew();
    await this.typeName(name);
    await this.chooseTrigger(trigger);
    // The backend refuses a workflow with no action.
    if (trigger === 'finalized') await this.addPromptStep(0);
    else await this.addIntegrationStep(0);
    await this.saveEditor();
    return waitForIdByName((n) => this.idByName(n), name, 'workflow');
  },

  /** Flips one row's Enabled switch and waits for the reloaded list to report
   *  the new state. Enabled rows sort first. */
  async setEnabled(id: string, on: boolean) {
    await setCheckbox(this.enabled(id), on, 'the workflow row');
  },

  /** Deletes one workflow through its confirm dialog. */
  async deleteWorkflow(id: string) {
    await clickRowMenuItem(`wf-menu-${id}`, `wf-delete-${id}`);
    await confirmDialog.acceptTyping();
    await expect(this.edit(id)).not.toBeExisting();
  },

  /** Removes every workflow carrying this name when one is listed. A workflow
   *  outlives the spec file that created it, so this is what a `finally`
   *  calls; it is idempotent and copes with an editor or dialog left open by a
   *  failed case. */
  async ensureAbsent(name: string) {
    await ensureAbsent({
      what: 'workflow',
      name,
      open: async () => {
        await nav.go('tasks');
        if (await this.cancel().isExisting()) await this.cancel().click();
      },
      idByName: (n) => this.idByName(n),
      deleteOne: (id) => this.deleteWorkflow(id),
    });
  },
};
