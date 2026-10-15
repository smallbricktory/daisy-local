// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Page objects for the session detail pane: header (date, title, action menu),
// the five tabs, the participants labeler and its modals, and the in-call chat.
import { browser, expect } from '@wdio/globals';
import {
  clickScrim, hover, optionValueByText, pressEnter, pressEscape, setNativeValue, tid, tids, typeInto,
  typeIntoEl,
  within, withinAll, type El,
} from './_shared.ts';
import { confirmDialog } from './shell.ts';

export type Tab = 'summary' | 'notes' | 'transcript' | 'participants' | 'chat';

/** The word the delete-recording dialog demands before it arms. */
const DELETE_WORD = 'DELETE';

const BACKSPACE = '\uE003';
const DELETE = '\uE017';

/** Replaces a field's text with keystrokes only, erasing character by character in both directions. */
async function replaceText(testid: string, text: string): Promise<void> {
  const field = tid(testid);
  await field.click();
  const erase = ((await field.getValue()) ?? '').length;
  if (erase > 0) await browser.keys([...Array(erase).fill(BACKSPACE), ...Array(erase).fill(DELETE)]);
  await browser.keys(text);
}

/** Sets a segmented date/time input; see `setNativeValue`. */
async function setPickerValue(testid: string, value: string): Promise<void> {
  await setNativeValue(testid, value);
}

export const session = {
  when: () => tid('session-when'),
  whenDate: () => tid('session-when-date'),
  whenTime: () => tid('session-when-time'),
  whenSave: () => tid('session-when-save'),
  whenCancel: () => tid('session-when-cancel'),
  title: () => tid('session-title'),
  /** "Recording paused while the computer slept (…)" under the title. */
  sleepNotice: () => tid('session-sleep-notice'),
  /** The pane's tag box; the nearest focusable neighbour of the title. */
  tagInput: () => tid('tag-combobox-input'),
  tab: (t: Tab) => tid(`session-tab-${t}`),
  actions: () => tid('session-actions'),
  action: (slug: string) => tid(`session-action-${slug}`),
  /** The pane's one error line, wherever the open tab draws it. */
  error: () => tid('session-error'),
  notesEditor: () => tid('md-editor'),
  /** The editable surface inside the notes editor. */
  notesInput: () => tid('md-editor').$('[contenteditable="true"]'),
  summaryBody: () => tid('session-summary-body'),
  summaryTldr: () => tid('session-summary-tldr'),
  summaryGenerate: () => tid('session-summary-generate'),
  summaryPaste: () => tid('session-summary-paste'),
  summaryPasteSave: () => tid('session-summary-paste-save'),
  summaryEditSave: () => tid('session-summary-edit-save'),
  summaryEditCancel: () => tid('session-summary-edit-cancel'),
  /** Both no-provider hints link to the provider picker. */
  summaryProvidersLink: () => tid('session-summary-providers'),
  chaptersProvidersLink: () => tid('session-chapters-providers'),
  /** Copy button for the active tab, keyed by what it copies. */
  copy: (kind: 'summary' | 'transcript') => tid(`session-copy-${kind}`),
  chaptersExtract: () => tid('session-chapters-extract'),
  chapter: (i: number) => tid(`session-chapter-${i}`),
  transcriptTurn: (i: number) => tid(`transcript-turn-${i}`),
  transcriptCtx: () => tid('transcript-ctx'),
  transcriptCtxSplit: () => tid('transcript-ctx-split'),
  transcriptCtxDeleteFrom: () => tid('transcript-ctx-delete-from'),
  chatInput: () => tid('chat-input'),
  chatSend: () => tid('chat-send'),
  chatMsg: (i: number) => tid(`chat-msg-${i}`),
  chatMsgs: () => tids('chat-msg-'),

  /** The heading text — the session's title, or its timestamp when untitled. */
  async titleText(): Promise<string> {
    return this.title().getText();
  },
  /** Clicks the heading and types over the field it turns into. Heading and
   *  field carry the same id, so the tag name is what tells them apart. */
  async startRename(t: string) {
    await this.title().click();
    await browser.waitUntil(async () => (await this.title().getTagName()) === 'input', {
      timeoutMsg: 'clicking the title did not open the rename field',
    });
    await replaceText('session-title', t);
    await expect(this.title()).toHaveValue(t);
  },
  /** The heading is back once the field has closed. */
  async expectHeading(t: string) {
    await browser.waitUntil(async () => (await this.title().getTagName()) === 'h1', {
      timeoutMsg: 'the rename field did not close',
    });
    await expect(this.title()).toHaveText(t);
  },
  /** Enter blurs the field, and the blur is what commits. */
  async rename(t: string) {
    await this.startRename(t);
    await pressEnter();
    await this.expectHeading(t);
  },
  /** Escape drops the edit; the heading keeps the name it had. */
  async renameAbort(t: string) {
    const before = await this.titleText();
    await this.startRename(t);
    await pressEscape();
    await this.expectHeading(before);
  },
  /** Clicking away commits the same as Enter does; the click lands on the tag box under the title. */
  async renameByBlur(t: string) {
    await this.startRename(t);
    await this.tagInput().click();
    await this.expectHeading(t);
  },
  async open(t: Tab) {
    await this.tab(t).click();
    await expect(this.tab(t)).toHaveElementClass('detail-tabs__tab--on');
  },
  /** Opens the date/time editor and reads back the values it seeded from the
   *  manifest. */
  async openWhen(): Promise<{ date: string; time: string }> {
    await this.when().click();
    await expect(this.whenDate()).toBeDisplayed();
    return { date: await this.whenDate().getValue(), time: await this.whenTime().getValue() };
  },
  async cancelWhen() {
    await this.whenCancel().click();
    await expect(this.when()).toBeDisplayed();
  },
  /** Commits a new recording date; the editor closes and the eyebrow returns. */
  async setWhen(date: string, time: string) {
    await setPickerValue('session-when-date', date);
    await setPickerValue('session-when-time', time);
    await expect(this.whenDate()).toHaveValue(date);
    await expect(this.whenTime()).toHaveValue(time);
    await this.whenSave().click();
    await expect(this.when()).toBeDisplayed();
  },
  /** Leaves the menu open. Idempotent. */
  async openActions() {
    if (!(await this.action('regen-transcript').isExisting())) await this.actions().click();
    await expect(this.action('regen-transcript')).toBeDisplayed();
  },
  async closeActions() {
    await pressEscape();
    await expect(this.action('regen-transcript')).not.toBeExisting();
  },
  /** Opens the menu and fires one item. */
  async openAction(slug: string) {
    await this.openActions();
    await expect(this.action(slug)).toBeEnabled();
    await this.action(slug).click();
  },
  /** Opens the menu, checks an item is offered but inert, and closes it. */
  async expectActionDisabled(slug: string) {
    await this.openActions();
    await expect(this.action(slug)).toBeDisabled();
    await this.closeActions();
  },
  /** Opens the Send To… submenu and leaves it up. */
  async openSendTo() {
    await this.openActions();
    await hover(this.action('send-to'));
  },
  /** Pushes the session to one destination through that submenu. */
  async sendTo(integrationId: string) {
    await this.openSendTo();
    await expect(this.action(`send-to-${integrationId}`)).toBeDisplayed();
    await this.action(`send-to-${integrationId}`).click();
    await expect(this.action('regen-transcript')).not.toBeExisting();
  },
  /** Regenerates through the per-prompt submenu, which opens on hover. */
  async regenWithPrompt(promptId: string) {
    await this.openActions();
    await hover(this.action('regen-summary'));
    await expect(this.action(`regen-summary-${promptId}`)).toBeDisplayed();
    await this.action(`regen-summary-${promptId}`).click();
    await expect(this.action('regen-transcript')).not.toBeExisting();
  },
  /** Fills the paste-your-own-summary box. */
  async typeSummaryPaste(markdown: string) {
    await typeInto('session-summary-paste', markdown);
    await expect(this.summaryPaste()).toHaveValue(markdown);
  },
  /** Types a summary from another tool into the paste box and saves it. */
  async pasteSummary(markdown: string) {
    await this.typeSummaryPaste(markdown);
    await this.summaryPasteSave().click();
    await expect(this.summaryBody()).toBeDisplayed();
  },
  /** Opens the summary editor from the rendered summary and leaves it unchanged. */
  async openSummaryEditorAndCancel() {
    await this.summaryBody().click();
    await expect(this.summaryEditSave()).toBeDisplayed();
    await this.summaryEditCancel().click();
    await expect(this.summaryBody()).toBeDisplayed();
  },
  /** Appends to the notes editor (TipTap holds the caret; text is inserted at
   *  it). The notes autosave; leaving the session flushes a pending write. */
  async typeNotes(text: string) {
    await this.notesInput().click();
    await browser.waitUntil(async () => this.notesInput().isFocused(), {
      timeoutMsg: 'the notes editor never took focus',
    });
    await this.notesInput().addValue(text);
    await expect(this.notesEditor()).toHaveText(expect.stringContaining(text));
  },
  async extractChapters() {
    await this.chaptersExtract().click();
    await expect(this.chapter(0)).toBeDisplayed();
  },
  /** Right-clicks a transcript turn; the cut menu is anchored on it. */
  async openTurnMenu(i: number) {
    await this.transcriptTurn(i).click({ button: 'right' });
    await expect(this.transcriptCtx()).toBeDisplayed();
  },
  async closeTurnMenu() {
    await pressEscape();
    await expect(this.transcriptCtx()).not.toBeExisting();
  },
  /** Copies the active tab's text; the button reports the outcome itself. */
  async clickCopy(kind: 'summary' | 'transcript') {
    await this.copy(kind).click();
    await expect(this.copy(kind)).toHaveText(expect.stringContaining('Copied'));
  },
  /** Fills the chat box without sending. */
  async typeChat(text: string) {
    await typeInto('chat-input', text);
    await expect(this.chatInput()).toHaveValue(text);
  },
  /** Sends one chat turn and waits for the assistant bubble behind it. */
  async sendChat(text: string) {
    const before = await this.chatMsgs().length;
    await typeInto('chat-input', text);
    await this.chatSend().click();
    await expect(this.chatMsg(before)).toHaveText(expect.stringContaining(text));
    await expect(this.chatMsgs()).toBeElementsArrayOfSize(before + 2);
  },
  /** The dialog demands the word typed out before it arms. */
  async deleteRecording() {
    await this.openAction('delete-recording');
    await confirmDialog.acceptTyping(DELETE_WORD);
  },
  async cancelDeleteRecording() {
    await this.openAction('delete-recording');
    await confirmDialog.cancelOut();
  },
};

/** Both lookups, either against the document or inside one subtree. */
interface Scope {
  one: (id: string) => El;
  many: (prefix: string) => ReturnType<typeof tids>;
}

const DOCUMENT: Scope = { one: tid, many: tids };

function speakerLabeler(at: Scope) {
  return {
    unavailableBanner: () => at.one('diarization-unavailable-banner'),
    count: () => at.one('speakers-count'),
    track: () => at.one('speakers-track'),
    diarize: () => at.one('speakers-diarize'),
    recount: () => at.one('speakers-recount'),
    retrack: () => at.one('speakers-retrack'),
    rediarize: () => at.one('speakers-rediarize'),
    addParticipant: () => at.one('speakers-add-participant'),
    /** "+ Add" beside one calendar invitee not yet matched to a voice. */
    addInvitee: (i: number) => at.one(`speakers-add-open-${i}`),
    addContact: () => at.one('speakers-add-contact'),
    /** Whether the add-participant modal offers this filed voice. */
    async addContactOffers(name: string): Promise<boolean> {
      return (await optionValueByText('speakers-add-contact', name)) !== null;
    },
    addName: () => at.one('speakers-add-name'),
    addEmail: () => at.one('speakers-add-email'),
    addCancel: () => at.one('speakers-add-cancel'),
    addSubmit: () => at.one('speakers-add-submit'),
    /** Cluster ids are numbers on the wire; both spellings address the row. */
    row: (clusterId: number | string) => at.one(`speaker-row-${clusterId}`),
    rows: () => at.many('speaker-row-'),
    menu: (clusterId: number | string) => at.one(`speaker-menu-${clusterId}`),
    wrongMatch: (clusterId: number | string) => at.one(`speaker-wrong-${clusterId}`),
    removeRow: (clusterId: number | string) => at.one(`speaker-remove-${clusterId}`),
    labelScrim: () => at.one('speaker-label-scrim'),
    labelModal: () => at.one('speaker-label-modal'),
    labelSample: () => at.one('speaker-label-sample'),
    labelContact: () => at.one('speaker-label-contact'),
    labelName: () => at.one('speaker-label-name'),
    labelEmail: () => at.one('speaker-label-email'),
    labelEnroll: () => at.one('speaker-label-enroll'),
    labelBackfill: () => at.one('speaker-label-backfill'),
    labelRemoveCluster: () => at.one('speaker-label-remove-cluster'),
    labelClear: () => at.one('speaker-label-clear'),
    labelCancel: () => at.one('speaker-label-cancel'),
    labelSave: () => at.one('speaker-label-save'),

    /** Waits for a participant row carrying `name`. */
    async expectRow(name: string) {
      await browser.waitUntil(async () => {
        for (const el of await this.rows()) {
          if ((await el.getText()).includes(name)) return true;
        }
        return false;
      }, { timeoutMsg: `no participant row named "${name}"` });
    },
    /** Adds a manual participant; the new row carries the name. */
    async addManual(name: string, email: string) {
      await this.addParticipant().click();
      await expect(this.addSubmit()).toBeDisabled();
      await typeIntoEl(at.one('speakers-add-name'), name);
      await typeIntoEl(at.one('speakers-add-email'), email);
      await this.addSubmit().click();
      await this.expectRow(name);
    },
    async openAddAndCancel() {
      await this.addParticipant().click();
      await expect(this.addName()).toBeDisplayed();
      await this.addCancel().click();
      await expect(this.addName()).not.toBeExisting();
    },
    /** Names one voice cluster. `enroll` off keeps it a per-session label. */
    async label(clusterId: number | string, name: string, enroll = false) {
      await this.openLabel(clusterId);
      if (!enroll) {
        await this.labelEnroll().click();
        await expect(this.labelEnroll()).not.toBeSelected();
      }
      await typeIntoEl(at.one('speaker-label-name'), name);
      await this.labelSave().click();
      await expect(this.labelModal()).not.toBeExisting();
      await expect(this.row(clusterId)).toHaveText(expect.stringContaining(name));
    },
    async openLabel(clusterId: number | string) {
      await this.row(clusterId).click();
      await expect(this.labelModal()).toBeDisplayed();
    },
    async cancelLabel() {
      await this.labelCancel().click();
      await expect(this.labelModal()).not.toBeExisting();
    },
    /** Opens one row's kebab menu. */
    async openRowMenu(clusterId: number | string) {
      await this.menu(clusterId).click();
      await expect(this.removeRow(clusterId)).toBeDisplayed();
    },
    async closeRowMenu(clusterId: number | string) {
      await pressEscape();
      await expect(this.removeRow(clusterId)).not.toBeExisting();
    },
  };
}

export const speakers = {
  ...speakerLabeler(DOCUMENT),
  /** The same accessors resolved inside `root`. The label gate mounts a second
   *  labeler over the participants tab, so while it is open every `speaker-*`
   *  id exists twice; pass `labelGate.modal()` to address the one on top. */
  in: (root: El) => speakerLabeler({
    one: (id: string) => within(root, id),
    many: (prefix: string) => withinAll(root, prefix),
  }),
};

/** The tag row under the title: attached chips, and the combobox that attaches
 *  an existing tag or creates one in place. The chip's pencil opens the tag's
 *  prompt/vocabulary modal. */
export const tagging = {
  input: () => tid('tag-combobox-input'),
  option: (id: string) => tid(`tag-combobox-option-${id}`),
  openCreate: () => tid('tag-combobox-open-create'),
  createForm: () => tid('tag-combobox-create-form'),
  create: () => tid('tag-combobox-create'),
  createCancel: () => tid('tag-combobox-cancel'),
  /** Swatch by hex digits without the `#`. */
  swatch: (hex: string) => tid(`tag-color-${hex}`),
  colorNative: () => tid('tag-color-native'),
  colorHex: () => tid('tag-color-hex'),
  chipEdit: (id: string) => tid(`tag-chip-edit-${id}`),
  chipRemove: (id: string) => tid(`tag-chip-remove-${id}`),
  /** The remove control of the chip carrying this tag name. */
  chipRemoveByName: (name: string) => browser.$(`[data-testid^="tag-chip-remove-"][title="Remove ${name}"]`),
  promptScrim: () => tid('tag-prompt-scrim'),
  promptModal: () => tid('tag-prompt'),
  promptDirective: () => tid('tag-prompt-directive'),
  promptVocab: () => tid('tag-prompt-vocab'),
  promptSave: () => tid('tag-prompt-save'),
  promptCancel: () => tid('tag-prompt-cancel'),
  /** The picker's Suggested section: the pick control and the × beside it. */
  suggested: (tagId: string) => tid(`tag-combobox-suggested-${tagId}`),
  dismissSuggested: (tagId: string) => tid(`tag-combobox-dismiss-${tagId}`),
  /** The open menu's own children in order, as class names: the section
   *  labels and the rule between them read off the same list the reader sees. */
  async popStructure(): Promise<string[]> {
    return browser.execute(() => {
      const pop = document.querySelector('.tag-combobox__pop');
      if (!pop) return [];
      return [...pop.children].map((el) =>
        el.className.includes('tag-combobox__section')
          ? `section:${el.textContent?.trim()}`
          : el.className.includes('menu__sep')
            ? 'sep'
            : 'item');
    });
  },
  /** Opens the menu with an empty query, which lists the Suggested section. */
  async openPicker() {
    await browser.execute(() => (document.activeElement as HTMLElement | null)?.blur());
    await this.input().click();
  },

  /** Id of the attached chip carrying this name, read off its remove control. */
  async idByName(name: string): Promise<string> {
    const chip = this.chipRemoveByName(name);
    await expect(chip).toBeExisting();
    return ((await chip.getAttribute('data-testid')) ?? '').replace('tag-chip-remove-', '');
  },
  /** Types a name no tag carries and opens the inline create form. */
  async startCreate(name: string) {
    await typeIntoEl(this.input(), name);
    await this.openCreate().click();
    await expect(this.createForm()).toBeDisplayed();
  },
  /** Creates the typed tag; it lands as a chip and the id comes back. */
  async finishCreate(name: string): Promise<string> {
    await this.create().click();
    await expect(this.createForm()).not.toBeExisting();
    return this.idByName(name);
  },
  /** Focusing the box lists the tags not yet attached; picking one attaches it. */
  async attach(id: string) {
    await this.openPicker();
    await this.option(id).click();
    await expect(this.chipRemove(id)).toBeExisting();
  },
  async remove(id: string) {
    await this.chipRemove(id).click();
    await expect(this.chipRemove(id)).not.toBeExisting();
  },
  async openPrompt(id: string) {
    await this.chipEdit(id).click();
    await expect(this.promptModal()).toBeDisplayed();
  },
  /** A click on the scrim, off the modal, closes it. */
  async closePromptByScrim() {
    await clickScrim('tag-prompt-scrim');
    await expect(this.promptModal()).not.toBeExisting();
  },
};

/** The label-speakers modal the needs-labels toast opens. */
export const labelGate = {
  scrim: () => tid('sstat-scrim'),
  modal: () => tid('sstat-modal'),
  save: () => tid('sstat-save'),
  later: () => tid('sstat-later'),
};

/** The "tags from last time" chip. The summary pane mounts it for a session
 *  that carries no tags yet; the recorder mounts the same component for a
 *  session that does not exist yet, where Apply only seeds the pre-record
 *  picks. */
export const lastTimeChip = {
  toggle: () => tid('last-time-chip-toggle'),
  tag: (tagId: string) => tid(`last-time-tag-${tagId}`),
  apply: () => tid('last-time-apply'),
  /** Opens the panel of per-tag toggles. */
  async expand() {
    await this.toggle().click();
    await expect(this.apply()).toBeDisplayed();
  },
};

/** The session's audio player. A seek sets `currentTime` as the scrubber
 *  does, then plays while a Web Audio analyser reads the output level. */
export const player = {
  audio: () => tid('session-playback'),
  async waitReady(timeout = 30_000) {
    await this.audio().waitForExist({ timeout });
    await browser.waitUntil(
      () => browser.execute(() => {
        const a = document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]');
        return !!a && a.readyState >= 1 && Number.isFinite(a.duration);
      }),
      { timeout, timeoutMsg: 'the player never reported its duration' },
    );
  },
  async src(): Promise<string> {
    return browser.execute(() => document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]')?.src ?? '');
  },
  async duration(): Promise<number> {
    return browser.execute(() => document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]')?.duration ?? Number.NaN);
  },
  /** Seeks to `target`, plays `playMs` and pauses. Returns `currentTime`
   *  once the seek settles and after the play, plus the signal level the
   *  element puts out every ~20 ms of the play (elapsed ms, RMS), read
   *  through a Web Audio analyser that keeps the output silent. */
  async seek(target: number, playMs = 3_000): Promise<{ seeked: number; played: number; playedMs: number; event: string; levels: Array<[number, number]> }> {
    return browser.execute(async (t: number, ms: number) => {
      const a = document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]');
      if (!a) return { seeked: Number.NaN, played: Number.NaN, playedMs: 0, event: 'no player', levels: [] };
      type Tap = { ctx: AudioContext; an: AnalyserNode; el: HTMLAudioElement };
      const w = window as unknown as { __daisySeekTap?: Tap };
      if (!w.__daisySeekTap || w.__daisySeekTap.el !== a) {
        const ctx = w.__daisySeekTap?.ctx ?? new AudioContext();
        const an = ctx.createAnalyser();
        an.fftSize = 1024;
        const mute = ctx.createGain();
        mute.gain.value = 0;
        ctx.createMediaElementSource(a).connect(an);
        an.connect(mute);
        mute.connect(ctx.destination);
        w.__daisySeekTap = { ctx, an, el: a };
      }
      const { ctx, an } = w.__daisySeekTap;
      await ctx.resume();
      a.muted = false;
      a.volume = 1;
      const settled = new Promise<string>((done) => {
        const timer = setTimeout(() => done('timeout'), 20_000);
        a.addEventListener('seeked', () => { clearTimeout(timer); done('seeked'); }, { once: true });
      });
      a.currentTime = t;
      const event = await settled;
      const seeked = a.currentTime;
      const buf = new Float32Array(an.fftSize);
      const levels: Array<[number, number]> = [];
      await a.play();
      const t0 = performance.now();
      while (performance.now() - t0 < ms) {
        an.getFloatTimeDomainData(buf);
        let sum = 0;
        for (const x of buf) sum += x * x;
        levels.push([performance.now() - t0, Math.sqrt(sum / buf.length)]);
        await new Promise((r) => setTimeout(r, 20));
      }
      a.pause();
      return { seeked, played: a.currentTime, playedMs: performance.now() - t0, event, levels };
    }, target, playMs);
  },
  /** The buffered ranges, in seconds. */
  async buffered(): Promise<Array<[number, number]>> {
    const flat = await browser.execute(() => {
      const out: number[] = [];
      const b = document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]')?.buffered;
      for (let i = 0; b && i < b.length; i++) out.push(b.start(i), b.end(i));
      return out;
    });
    const ranges: Array<[number, number]> = [];
    for (let i = 0; i + 1 < flat.length; i += 2) ranges.push([flat[i], flat[i + 1]]);
    return ranges;
  },
  /** The media error, `code: message`, or null. */
  async error(): Promise<string | null> {
    return browser.execute(() => {
      const e = document.querySelector<HTMLAudioElement>('[data-testid="session-playback"]')?.error;
      return e ? `${e.code}: ${e.message}` : null;
    });
  },
};
