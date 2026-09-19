// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Every item in the session action menu, driven for its effect: the command it
// invokes and the arguments it carries, the dialog it puts up, or the editor it
// opens. Presence of the item is spec 21's job.
import { browser, expect } from '@wdio/globals';
import { isNative } from '../env.ts';
import { bootWorld } from '../fixtures/install.ts';
import { callsOf, clearCalls, mockInvoke, mockInvokeWith, restoreInvoke } from '../fixtures/invoke.ts';
import { confirmDialog, nav, toast } from '../pages/shell.ts';
import { library, newMeeting } from '../pages/library.ts';
import { session } from '../pages/session.ts';
import { hover } from '../pages/_shared.ts';

/** Only the browser world seeds a session that already carries a summary; the
 *  native sandbox's session is a typed note with no summary to edit,
 *  regenerate or export. Native mocks a ready AI provider, so items gated on
 *  a provider (not on a summary) are live on both. */
const HAS_SUMMARY = !isNative();
const PROVIDER_STATUS = 'summary_provider_status';
const CONFIGURED = { state: 'Configured', provider: 'local', hint: null };

const NOTE_TITLE = 'Action menu target';
/** A second row, so the pane can be left and re-entered. */
const OTHER_TITLE = 'Action menu bystander';
const SESSION_STATUS = 'session-status';

const REPAIR = 'repair_session';
const TRANSCRIBE = 'transcribe';
const DEDUP = 'dedup';
const COMPLETE = 'mark_session_complete';
const DIARIZE = 'diarize_session';
const POLISH = 'polish';
const SPLIT = 'split_session_at';
const TRUNCATE = 'truncate_session_after';
const LIST_INTEGRATIONS = 'list_integrations';
const PUSH = 'integration_push';
const META = 'session_meta_get';
const CHAT_LOAD = 'live_chat_load';
const CHAT_DELETE = 'live_chat_delete';
const SAVE_DIALOG = 'plugin:dialog|save';
const SAVE_TEXT = 'save_text_file';

const EXPORT_PATH = '/tmp/daisy-e2e-export.md';

/** Two destinations, so choosing one proves which id travelled. */
const DESTINATIONS = [
  {
    id: 'i-e2e-ops', name: 'Ops webhook', enabled: true, kind: 'webhook',
    webhook_url: 'https://hooks.example.test/ops', auth_kind: 'none', auth_header_name: null,
    payloads: { summary: true, notes: false, transcript: false },
  },
  {
    id: 'i-e2e-crm', name: 'CRM notes', enabled: true, kind: 'webhook',
    webhook_url: 'https://hooks.example.test/crm', auth_kind: 'none', auth_header_name: null,
    payloads: { summary: true, notes: true, transcript: false },
  },
];

let targetId = '';
let otherId = '';

/** Re-opens the pane from the library by way of another row, which is what
 *  re-runs its per-session loads (meta, chat) against whatever is mocked now.
 *  Clicking the row that is already open changes nothing. */
async function reopenTarget(): Promise<void> {
  await nav.go('library');
  await library.openRow(otherId);
  await library.openRow(targetId);
}

describe('session action menu', () => {
  before(async () => {
    await bootWorld();
    // The pane reads the provider status when it opens a session, and a new
    // note opens as it is created, so the status is in place before that.
    if (isNative()) await mockInvoke(PROVIDER_STATUS, CONFIGURED);
    await nav.go('library');
    if (isNative()) {
      await newMeeting.createNote(OTHER_TITLE, 'Agenda: none');
      await library.expectRow(OTHER_TITLE);
      if (await toast.byId(SESSION_STATUS).isExisting()) await toast.dismissById(SESSION_STATUS);
      await newMeeting.createNote(NOTE_TITLE, 'Agenda: renewal, headcount');
      await library.expectRow(NOTE_TITLE);
      targetId = await library.rowIdByTitle(NOTE_TITLE);
      if (await toast.byId(SESSION_STATUS).isExisting()) await toast.dismissById(SESSION_STATUS);
    } else {
      targetId = await library.firstRowId();
    }
    const other = (await library.rowIds()).filter((id) => id !== targetId)[0];
    if (!other) throw new Error('reopenTarget needs a second library row');
    otherId = other;
    await library.openRow(targetId);
  });

  beforeEach(async () => {
    await clearCalls();
    if (isNative()) await mockInvoke(PROVIDER_STATUS, CONFIGURED);
  });

  after(async () => {
    if (isNative()) await restoreInvoke(PROVIDER_STATUS);
  });

  it('closing the menu with Escape fires nothing', async () => {
    await mockInvoke(REPAIR, 0);
    try {
      await session.openActions();
      await session.closeActions();
      expect(await callsOf(REPAIR)).toHaveLength(0);
    } finally {
      await restoreInvoke(REPAIR);
    }
  });

  it('edit summary opens the editor over the summary tab', async () => {
    if (!HAS_SUMMARY) {
      // Nothing to edit until a summary exists.
      await session.expectActionDisabled('edit-summary');
      return;
    }
    // From another tab, so the switch back to summary is part of the effect.
    await session.open('transcript');
    await session.openAction('edit-summary');
    await expect(session.summaryEditSave()).toBeDisplayed();
    await expect(session.notesEditor()).toBeDisplayed();
    await session.summaryEditCancel().click();
    await expect(session.summaryBody()).toBeDisplayed();
  });

  it('the regen submenu offers every prompt', async () => {
    if (!HAS_SUMMARY) {
      await session.expectActionDisabled('regen-summary');
      return;
    }
    await session.openActions();
    await hover(session.action('regen-summary'));
    for (const id of ['builtin:daisy', 'builtin:zoom', 'builtin:otter', 'u1']) {
      await expect(session.action(`regen-summary-${id}`)).toBeDisplayed();
    }
    await session.closeActions();
  });

  it('regenerating the transcript asks first, and cancelling runs nothing', async () => {
    await mockInvoke(TRANSCRIBE, 42);
    try {
      await session.openAction('regen-transcript');
      await expect(confirmDialog.modal())
        .toHaveText(expect.stringContaining('Regenerate transcript?'));
      await confirmDialog.cancelOut();
      expect(await callsOf(TRANSCRIBE)).toHaveLength(0);
    } finally {
      await restoreInvoke(TRANSCRIBE);
    }
  });

  it('accepting re-transcribes, re-dedups and re-stamps the session', async () => {
    await mockInvoke(TRANSCRIBE, 42);
    await mockInvoke(DEDUP, { dropped: 3, kept: 39 });
    await mockInvoke(COMPLETE, null);
    // Re-diarization follows the dedup; it owns its own toast and is not what
    // this test is about.
    await mockInvoke(DIARIZE, { speakers: 2, segments_labeled: 12, segments_seen: 12 });
    try {
      await session.openAction('regen-transcript');
      await confirmDialog.acceptTyping();
      await browser.waitUntil(async () => (await callsOf(COMPLETE)).length === 1, {
        timeoutMsg: 'the re-transcribe chain never reached mark_session_complete',
      });
      expect(await callsOf(TRANSCRIBE)).toEqual([{ req: { session_id: targetId } }]);
      expect(await callsOf(DEDUP)).toEqual([{ req: { session_id: targetId } }]);
      expect(await callsOf(COMPLETE)).toEqual([{ sessionId: targetId }]);
    } finally {
      for (const c of [TRANSCRIBE, DEDUP, COMPLETE, DIARIZE]) await restoreInvoke(c);
    }
  });

  it('polishing asks first, then reports what it cleaned', async () => {
    await mockInvoke(POLISH, { batches: 2, segments_polished: 12, segments_unchanged: 27, failed_batches: 0 });
    try {
      await session.openAction('polish');
      await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('Polish transcript?'));
      await confirmDialog.cancelOut();
      expect(await callsOf(POLISH)).toHaveLength(0);

      await session.openAction('polish');
      await confirmDialog.acceptTyping();
      await expect(toast.byId(`polish:${targetId}`))
        .toHaveText(expect.stringContaining('12 segments cleaned'));
      expect(await callsOf(POLISH)).toEqual([{ req: { session_id: targetId } }]);
    } finally {
      await restoreInvoke(POLISH);
    }
  });

  it('repair says so when every file is already present', async () => {
    await session.open('summary');
    await mockInvoke(REPAIR, 0);
    try {
      await session.openAction('repair');
      await expect(session.error()).toHaveText('Nothing to repair — all files present.');
      expect(await callsOf(REPAIR)).toEqual([{ sessionId: targetId }]);
    } finally {
      await restoreInvoke(REPAIR);
    }
  });

  it('repair that rebuilt something says nothing', async () => {
    await session.open('summary');
    await mockInvoke(REPAIR, 2);
    try {
      await session.openAction('repair');
      await browser.waitUntil(async () => (await callsOf(REPAIR)).length === 1, {
        timeoutMsg: 'repair never ran',
      });
      await expect(session.error()).not.toBeExisting();
    } finally {
      await restoreInvoke(REPAIR);
    }
  });

  it('split and delete-from send the user to the transcript, cutting nothing', async () => {
    await mockInvoke(SPLIT, { new_session_id: 'nope', chunks_moved: 0, chunks_kept: 0 });
    await mockInvoke(TRUNCATE, { chunks_removed: 0, chunks_kept: 0, duration_seconds: 0 });
    try {
      await session.openAction('split');
      await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('Split into a new meeting'));
      await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('right-click'));
      await confirmDialog.acceptTyping();

      await session.openAction('delete-from');
      await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('Delete from a point'));
      await confirmDialog.acceptTyping();

      expect(await callsOf(SPLIT)).toHaveLength(0);
      expect(await callsOf(TRUNCATE)).toHaveLength(0);
    } finally {
      await restoreInvoke(SPLIT);
      await restoreInvoke(TRUNCATE);
    }
  });

  it('Send To… pushes to the destination that was picked', async () => {
    await mockInvoke(LIST_INTEGRATIONS, DESTINATIONS);
    await mockInvoke(PUSH, null);
    try {
      await session.sendTo('i-e2e-crm');
      await browser.waitUntil(async () => (await callsOf(PUSH)).length === 1, {
        timeoutMsg: 'Send To… never pushed',
      });
      expect(await callsOf(PUSH))
        .toEqual([{ sessionId: targetId, integrationId: 'i-e2e-crm' }]);
    } finally {
      await restoreInvoke(LIST_INTEGRATIONS);
      await restoreInvoke(PUSH);
    }
  });

  it('a destination already sent to is ticked', async () => {
    await mockInvoke(LIST_INTEGRATIONS, DESTINATIONS);
    await mockInvokeWith(META, (args) => ({
      session_id: String((args ?? {}).sessionId ?? ''),
      title: 'Action menu target',
      tag_ids: [],
      attendees: [{ display_name: 'Rowan Hale', role: 'self' }],
      has_notes: false,
      has_summary: true,
      recording_segments: 1,
      sent_integration_ids: ['i-e2e-ops'],
      recording_kind: null,
    }));
    try {
      await reopenTarget();
      await session.openSendTo();
      await expect(session.action('send-to-i-e2e-ops').$('[aria-label="already sent"]'))
        .toBeDisplayed();
      await expect(session.action('send-to-i-e2e-crm').$('[aria-label="already sent"]'))
        .not.toBeExisting();
      await session.closeActions();
    } finally {
      await restoreInvoke(LIST_INTEGRATIONS);
      await restoreInvoke(META);
      await reopenTarget();
    }
  });

  it('with no destinations defined the submenu points at Settings', async () => {
    await mockInvoke(LIST_INTEGRATIONS, []);
    await mockInvoke(PUSH, null);
    try {
      await session.openSendTo();
      await expect(session.action('send-to-none')).toBeDisplayed();
      await expect(session.action('send-to-i-e2e-ops')).not.toBeExisting();
      await session.closeActions();
      expect(await callsOf(PUSH)).toHaveLength(0);
    } finally {
      await restoreInvoke(LIST_INTEGRATIONS);
      await restoreInvoke(PUSH);
    }
  });

  it('export .md writes the summary to the file that was chosen', async () => {
    if (!HAS_SUMMARY) {
      // There is nothing to export until a summary exists.
      await session.expectActionDisabled('export-md');
      return;
    }
    await mockInvoke(SAVE_DIALOG, EXPORT_PATH);
    await mockInvoke(SAVE_TEXT, null);
    try {
      await session.openAction('export-md');
      await browser.waitUntil(async () => (await callsOf(SAVE_TEXT)).length === 1, {
        timeoutMsg: 'Export .md never wrote the file',
      });
      const [write] = await callsOf(SAVE_TEXT);
      expect(write?.path).toBe(EXPORT_PATH);
      expect(String(write?.contents ?? '')).toContain('#');
      expect(String(write?.contents ?? '').length).toBeGreaterThan(20);
    } finally {
      await restoreInvoke(SAVE_DIALOG);
      await restoreInvoke(SAVE_TEXT);
    }
  });

  it('a cancelled export writes nothing', async () => {
    if (!HAS_SUMMARY) return;
    await mockInvoke(SAVE_DIALOG, null);
    await mockInvoke(SAVE_TEXT, null);
    try {
      await session.openAction('export-md');
      await browser.waitUntil(async () => (await callsOf(SAVE_DIALOG)).length === 1, {
        timeoutMsg: 'Export .md never opened the save dialog',
      });
      expect(await callsOf(SAVE_TEXT)).toHaveLength(0);
    } finally {
      await restoreInvoke(SAVE_DIALOG);
      await restoreInvoke(SAVE_TEXT);
    }
  });

  it('delete chat is offered only once a thread exists, and deletes it', async () => {
    // No thread: the item is not in the menu at all.
    await session.openActions();
    await expect(session.action('delete-chat')).not.toBeExisting();
    await session.closeActions();

    await mockInvokeWith(CHAT_LOAD, () => ({
      messages: [
        { role: 'user', content: 'What did Rowan commit to?', ts: 1 },
        { role: 'assistant', content: 'Rowan Hale took the renewal draft.', ts: 2 },
      ],
      transcript_cursor_ms: 0,
    }));
    await mockInvoke(CHAT_DELETE, null);
    try {
      await reopenTarget();
      await session.openAction('delete-chat');
      await expect(confirmDialog.modal()).toHaveText(expect.stringContaining('Delete this chat?'));
      await confirmDialog.cancelOut();
      expect(await callsOf(CHAT_DELETE)).toHaveLength(0);

      await session.openAction('delete-chat');
      await confirmDialog.acceptTyping();
      await browser.waitUntil(async () => (await callsOf(CHAT_DELETE)).length === 1, {
        timeoutMsg: 'the chat was never deleted',
      });
      expect(await callsOf(CHAT_DELETE)).toEqual([{ sessionId: targetId }]);
      // The item leaves the menu with the thread it acted on.
      await session.openActions();
      await expect(session.action('delete-chat')).not.toBeExisting();
      await session.closeActions();
    } finally {
      await restoreInvoke(CHAT_LOAD);
      await restoreInvoke(CHAT_DELETE);
      await reopenTarget();
    }
  });
});
