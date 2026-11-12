// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, mockInvoke, mockInvokeWith, restoreInvoke } from '../fixtures/invoke.ts';
import { confirmDialog, nav, toast } from '../pages/shell.ts';
import { library, newMeeting } from '../pages/library.ts';
import { session, speakers, tagging } from '../pages/session.ts';
import { aiStatus, tags as settingsTags } from '../pages/settings.ts';
import { typeIntoEl } from '../pages/_shared.ts';

/** The native sandbox's target is a freshly created note: no seeded
 *  transcript, chapters or chat, and a summary only once the paste test has
 *  saved one. Native mocks a ready AI provider, so provider-gated controls
 *  are live there; what stays native-skipped below needs canned content
 *  instead. */
/** A seeded browser session that has no summary yet. */
const UNSUMMARIZED = 's-003';
const CONFIGURED = { state: 'Configured', provider: 'local', hint: null };

const TITLE = 'Detail target';
const RENAMED = 'Renamed by e2e';
const NOTES_LINE = 'Follow up with the vendor';
const PASTED_SUMMARY = '## TL;DR\nPasted by e2e';
const SAVE_TEXT = 'save_text_file';
const REGENERATE = 'summary_regenerate';
const READ_SESSION = 'read_session';
const A_TAG = 'E2E inline tag';
/** The lifecycle toast 20-library's import leaves standing in native. */
const SESSION_STATUS = 'session-status';

/** Timestamped turns; the transcript cut menu only arms on a turn that carries
 *  one. The browser world's default transcript has no timestamps. */
const TRANSCRIPT_MD = [
  '[00:00:03] **Jordan Rivera**: Let us start with the roadmap.',
  '',
  '[00:01:12] **Me**: The two big items are onboarding and the vendor migration.',
  '',
  '[00:02:40] **Jordan Rivera**: Then I will book the vendor follow-up.',
].join('\n');

/** The session under test, and another one to switch to and back. */
let targetId = '';
let otherId = '';

describe('session detail', () => {
  before(async () => {
    await bootWorld({ transcriptMd: TRANSCRIPT_MD });
    if (isNative()) {
      // The pane picks its first tab from the provider status when it opens
      // a session, and a new note opens as it is created, so the status is
      // in place first.
      await aiStatus.pin(CONFIGURED);
      await nav.go('library');
      await newMeeting.createNote(TITLE, 'Agenda: pricing, timeline');
      await library.expectRow(TITLE);
      targetId = await library.rowIdByTitle(TITLE);
      // The import's status toast sits over the pane's bottom-right corner
      // for the rest of the run.
      if (await toast.byId(SESSION_STATUS).isExisting()) await toast.dismissById(SESSION_STATUS);
    } else {
      await nav.go('library');
      targetId = await library.firstRowId();
    }
    otherId = (await library.rowIds()).filter((id) => id !== targetId)[0];
    await library.openRow(targetId);
  });

  // Native runs as a machine whose AI provider is ready. A test that wants
  // another provider state pins it over this and puts this back itself.
  beforeEach(async () => {
    if (isNative()) await aiStatus.pin(CONFIGURED);
  });
  after(async () => {
    if (isNative()) await aiStatus.release();
  });

  it('the title commits on Enter, reverts on Escape, and commits on blur', async () => {
    await session.rename(RENAMED);
    await session.renameAbort('Should not stick');
    await session.renameByBlur(`${RENAMED} again`);
  });

  it('every tab opens', async () => {
    for (const t of ['notes', 'transcript', 'participants', 'chat', 'summary'] as const) {
      await session.open(t);
    }
  });

  it('the recording date and time are editable in place', async () => {
    const seeded = await session.openWhen();
    expect(seeded.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    await session.cancelWhen();

    await session.openWhen();
    await session.setWhen('2026-03-04', '09:30');
    const saved = await session.openWhen();
    expect(saved).toEqual({ date: '2026-03-04', time: '09:30' });
    await session.cancelWhen();
  });

  it('notes survive leaving the session and coming back', async () => {
    await session.open('notes');
    await session.typeNotes(NOTES_LINE);
    // Another session in between forces the notes to be re-read from the
    // backend rather than shown from component state.
    await library.openRow(otherId);
    await library.openRow(targetId);
    await session.open('notes');
    await expect(session.notesEditor()).toHaveText(expect.stringContaining(NOTES_LINE));
  });

  /** No provider: the summary tab offers the paste box, and a saved paste
   *  becomes the session summary — a hand-written one, with no TL;DR callout
   *  to render empty. */
  async function pasteBecomesTheSummary() {
    await session.open('summary');
    await expect(session.summaryGenerate()).not.toBeExisting();
    await expect(session.summaryPaste()).toBeDisplayed();
    await expect(session.summaryPasteSave()).toBeDisabled();
    await session.pasteSummary(PASTED_SUMMARY);
    await expect(session.summaryBody()).toHaveText(expect.stringContaining('Pasted by e2e'));
    await expect(session.summaryTldr()).not.toBeExisting();
    await expect(session.summaryPaste()).not.toBeExisting();
  }

  it('a pasted summary becomes the session summary when there is no provider', async () => {
    await aiStatus.pin({ state: 'None', provider: null, hint: null });
    try {
      // The native target carries no summary yet, and re-opening it routes
      // the pane under the pinned status. Browser mode needs an unsummarized
      // row, since its default target has one.
      if (isNative()) {
        await library.openRow(otherId);
        await library.openRow(targetId);
      } else {
        await library.openRow(UNSUMMARIZED);
      }
      await pasteBecomesTheSummary();
    } finally {
      if (isNative()) {
        await aiStatus.pin(CONFIGURED);
        await library.openRow(otherId);
      } else {
        await aiStatus.release();
      }
      await library.openRow(targetId);
    }
  });

  it('a generated summary renders its TL;DR and opens the editor', async function () {
    // Needs the seeded browser summary; the native note has none to render.
    if (isNative()) this.skip();
    await session.open('summary');
    await expect(session.summaryPaste()).not.toBeExisting();
    await expect(session.summaryTldr()).toHaveText(expect.stringContaining('The team agreed'));
    await expect(session.summaryBody()).toHaveText(expect.stringContaining('The team agreed'));
    await session.openSummaryEditorAndCancel();
  });

  it('Copy reports what it put on the clipboard', async () => {
    await session.open('summary');
    await session.clickCopy('summary');
  });

  it('the action menu offers every action', async () => {
    await session.openActions();
    for (const slug of [
      'edit-summary', 'regen-summary', 'regen-transcript', 'polish', 'repair',
      'split', 'delete-from', 'send-to', 'export-md', 'delete-recording',
    ]) {
      await expect(session.action(slug)).toBeExisting();
    }
    await session.closeActions();
  });

  it('regenerating the summary runs the chosen prompt', async () => {
    await mockInvoke(REGENERATE, null);
    try {
      await session.regenWithPrompt('builtin:daisy');
      // Native regenerates the summary pasted above, a hand-written one, so
      // the overwrite is confirmed first.
      if (isNative()) {
        await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('overwrite your hand-edited summary'));
        await confirmDialog.acceptTyping();
      }
      await expect(toast.byId(`regen-summary:${targetId}`))
        .toHaveText(expect.stringContaining('Summary ready'));
      expect(await callsOf(REGENERATE)).toHaveLength(1);
    } finally {
      await restoreInvoke(REGENERATE);
    }
  });

  it('export .md asks the backend to open the save dialog', async () => {
    // null = the user cancelled the dialog.
    await mockInvoke(SAVE_TEXT, null);
    try {
      await session.openAction('export-md');
      await browser.waitUntil(async () => (await callsOf(SAVE_TEXT)).length === 1, {
        timeoutMsg: 'Export .md never asked the backend to save',
      });
    } finally {
      await restoreInvoke(SAVE_TEXT);
    }
  });

  it('chapters extract from the transcript', async () => {
    await session.open('transcript');
    if (isNative()) {
      // The native note carries no transcript, and chapters are cut from one.
      await expect(session.chaptersExtract()).toBeDisabled();
      return;
    }
    await session.extractChapters();
    await expect(session.chapter(0)).toHaveText(expect.stringContaining('Opening'));
  });

  /** Keeps the world's AI task in the registry until the spec clears it. */
  async function holdAiJob(hold: boolean): Promise<void> {
    await browser.execute((h: boolean) => {
      (window as unknown as { __e2e_world: { data: { aiJobHold: boolean } } })
        .__e2e_world.data.aiJobHold = h;
    }, hold);
  }

  it('an extraction in flight gates the other sessions and survives navigation', async function () {
    // Drives the world's job registry directly; native has no such fixture.
    if (isNative()) this.skip();
    await library.openRow(otherId);
    await session.open('transcript');
    await holdAiJob(true);
    try {
      await session.chaptersExtract().click();
      await expect(session.chaptersExtract()).toHaveText('Extracting…');

      // Another session's button is gated for as long as the task holds the
      // worker, and says why.
      await library.openRow(targetId);
      await session.open('transcript');
      await expect(session.chaptersExtract()).toBeDisabled();
      await expect(session.chaptersExtract())
        .toHaveAttribute('title', 'Another AI task is still running');

      // Back on the session that started it, the button is still running.
      await library.openRow(otherId);
      await session.open('transcript');
      await expect(session.chaptersExtract()).toHaveText('Extracting…');
    } finally {
      await holdAiJob(false);
    }
    await expect(session.chapter(0)).toHaveText(expect.stringContaining('Opening'));
    await library.openRow(targetId);
  });

  it('a transcript turn offers split and delete-from', async function () {
    // The native target is a note-only meeting: no transcript, no turns.
    if (isNative()) this.skip();
    await session.open('transcript');
    await session.openTurnMenu(1);
    await expect(session.transcriptCtxSplit()).toBeDisplayed();
    await expect(session.transcriptCtxDeleteFrom()).toBeDisplayed();
    await session.closeTurnMenu();
  });

  it('participants: add someone by hand and name a voice', async () => {
    await session.open('participants');
    if (isNative()) {
      // No clusters on a note-only meeting — the tab offers diarization
      // instead of a participant list.
      await expect(speakers.diarize()).toBeDisplayed();
      await expect(speakers.count()).toBeDisplayed();
      await expect(speakers.track()).toBeDisplayed();
      await expect(speakers.addParticipant()).not.toBeExisting();
      return;
    }
    await speakers.openAddAndCancel();
    await speakers.addManual('Casey Morgan', 'casey@example.com');
    await speakers.openLabel(1);
    // Scoped accessors reach into one labeler only: the modal holds the name
    // field, and the rows behind it stay outside.
    const inModal = speakers.in(speakers.labelModal());
    await expect(inModal.labelName()).toBeDisplayed();
    await expect(inModal.row(1)).not.toBeExisting();
    await speakers.cancelLabel();
    await speakers.label(1, 'Riley Chen');
    // A named cluster can be un-named again.
    await speakers.openLabel(1);
    await expect(speakers.labelClear()).toBeDisplayed();
    await speakers.cancelLabel();
    // Saving with the voiceprint box on files the voice, which moves the row
    // to Voice Matched — the only place the wrong-match undo is offered.
    await speakers.label(0, 'Sam Perkins', true);
    await speakers.openRowMenu(0);
    await expect(speakers.wrongMatch(0)).toBeDisplayed();
    await speakers.closeRowMenu(0);

    // A voice enrolled a moment ago is offered by the add-participant modal.
    await speakers.addParticipant().click();
    await expect(speakers.addContact()).toBeDisplayed();
    expect(await speakers.addContactOffers('Sam Perkins')).toBe(true);
    await speakers.addCancel().click();
  });

  it('the chat answers a question about this meeting', async () => {
    await session.open('chat');
    await expect(session.chatSend()).toBeDisabled();
    if (isNative()) {
      // No canned reply on native; that the box arms on typing is the whole UI here.
      await session.typeChat('What did we decide?');
      await expect(session.chatSend()).toBeEnabled();
      return;
    }
    await session.sendChat('What did we decide?');
    await expect(session.chatMsg(1)).toHaveText(expect.stringContaining('Noted.'));
  });

  it('participants: says so when diarization was unavailable for this recording', async () => {
    await mockInvokeWith(READ_SESSION, (args) => ({
      session_id: String(args?.sessionId ?? ''),
      manifest_json: {
        session_id: String(args?.sessionId ?? ''),
        created_at_unix_seconds: 1_700_000_000,
        chunks: [],
        diarization_unavailable: true,
      },
      transcript_md: null,
      has_transcript: false,
      has_dedup: false,
      has_summary: false,
    }));
    try {
      // The pane reads the session once per open; leaving and returning is
      // what makes it read the mocked view.
      await library.openRow(otherId);
      await library.openRow(targetId);
      await session.open('participants');
      await expect(speakers.unavailableBanner()).toBeDisplayed();
    } finally {
      await restoreInvoke(READ_SESSION);
      await library.openRow(otherId);
      await library.openRow(targetId);
    }
  });

  it('header: says when a system sleep paused the recording', async () => {
    await mockInvokeWith(READ_SESSION, (args) => ({
      session_id: String(args?.sessionId ?? ''),
      manifest_json: {
        session_id: String(args?.sessionId ?? ''),
        created_at_unix_seconds: 1_700_000_000,
        chunks: [],
        recording_segments: [
          { started_at_unix_seconds: 1_700_000_000, stopped_at_unix_seconds: 1_700_000_600, first_chunk_index: 1, last_chunk_index: 2 },
          { started_at_unix_seconds: 1_700_000_812, stopped_at_unix_seconds: 1_700_001_400, first_chunk_index: 3, last_chunk_index: 4, reason: 'system_sleep' },
        ],
      },
      transcript_md: null,
      has_transcript: false,
      has_dedup: false,
      has_summary: false,
    }));
    try {
      await library.openRow(otherId);
      await library.openRow(targetId);
      await expect(session.sleepNotice()).toHaveText(
        expect.stringContaining('Recording paused while the computer slept'),
      );
    } finally {
      await restoreInvoke(READ_SESSION);
      await library.openRow(otherId);
      await library.openRow(targetId);
    }
    await expect(session.sleepNotice()).not.toBeExisting();
  });

  it('tags: one is created in place, removed, attached again, and given a prompt', async () => {
    try {
      await tagging.startCreate(A_TAG);
      await tagging.swatch('3664E2').click();
      await expect(tagging.colorHex()).toHaveValue('#3664E2');
      await expect(tagging.colorNative()).toBeExisting();
      // A typed hex selects the matching swatch.
      await typeIntoEl(tagging.colorHex(), '#2BA13B');
      await expect(tagging.swatch('2BA13B')).toHaveElementClass(expect.stringContaining('--sel'));
      await tagging.createCancel().click();
      await expect(tagging.createForm()).not.toBeExisting();
      await tagging.openCreate().click();
      await expect(tagging.createForm()).toBeDisplayed();
      const id = await tagging.finishCreate(A_TAG);
      await tagging.remove(id);
      await tagging.attach(id);

      await tagging.openPrompt(id);
      await typeIntoEl(tagging.promptDirective(), 'Lead with decisions.');
      await typeIntoEl(tagging.promptVocab(), 'Northwind Logistics, NWL');
      await tagging.promptSave().click();
      await expect(tagging.promptModal()).not.toBeExisting();
      await tagging.openPrompt(id);
      await expect(tagging.promptDirective()).toHaveValue('Lead with decisions.');
      await expect(tagging.promptVocab()).toHaveValue('Northwind Logistics, NWL');
      await tagging.promptCancel().click();
      await expect(tagging.promptModal()).not.toBeExisting();
      await tagging.openPrompt(id);
      await tagging.closePromptByScrim();
      await tagging.remove(id);
    } finally {
      // The tag itself outlives the chip; Settings is where it is deleted.
      await settingsTags.ensureDeleted(A_TAG);
      await nav.go('library');
      await library.openRow(targetId);
    }
  });

  it('delete recording asks first: cancel keeps the row, accept removes it', async () => {
    await session.cancelDeleteRecording();
    await expect(library.row(targetId)).toBeExisting();
    await session.deleteRecording();
    await expect(library.row(targetId)).not.toBeExisting();
  });
});
