// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// User-facing strings of the session-encryption feature.

export const EXPORT_AUDIO_OPUS_LABEL = 'Export audio (.opus)';
export const EXPORT_AUDIO_WAV_LABEL = 'Export audio (.wav)';

// Settings › Recordings: the background seal of recordings older builds
// left unencrypted. `{done}` and `{total}` count recordings.
export const SEALING_TITLE = 'Encrypting your recordings';
export const SEALING_HINT = 'Daisy is encrypting recordings made before this update. You can keep working.';
export const SEALING_PROGRESS = '{done} of {total} recordings encrypted';

// Recovery key: shown once after the first unlock, from Settings › Profile
// after the passphrase, and asked for when recordings stay locked.
export const RECOVERY_TITLE = 'Your recovery key';
export const RECOVERY_INTRO = 'These five words reopen your recordings if you forget your passphrase or reset the vault. Write them down and keep them somewhere safe.';
export const RECOVERY_SAVED_LABEL = 'I saved my recovery key';
export const RECOVERY_DONE = 'Done';
export const RECOVERY_PASSPHRASE_PROMPT = 'Enter your passphrase to show the recovery key.';
export const RECOVERY_PASSPHRASE_LABEL = 'Passphrase';
export const RECOVERY_SHOW = 'Show recovery key';
export const RECOVERY_SHOW_FAILED = 'The passphrase is incorrect.';
export const RECOVERY_SETTINGS_GROUP = 'Recovery key';
export const RECOVERY_SETTINGS_BUTTON = 'Show recovery key…';
export const RECOVERY_SETTINGS_HINT = 'The recovery key reopens your recordings if the passphrase is forgotten.';

export const RECOVERY_ENTRY_TITLE = 'Enter your recovery key';
export const RECOVERY_ENTRY_INTRO = 'Your recordings stay locked until you enter the five words of your recovery key.';
export const RECOVERY_ENTRY_LABEL = 'Recovery key';
export const RECOVERY_ENTRY_SUBMIT = 'Reopen recordings';
export const RECOVERY_ENTRY_LATER = 'Not now';
export const RECOVERY_INCORRECT = 'The recovery key you entered is incorrect.';
export const RECOVERY_ALREADY_OPEN = 'The recordings these words open are already open.';

// Recovery screen, when the recovery key is lost: a new key starts and
// recording continues under it.
export const RECOVERY_START_NEW = 'I don\'t have my recovery key — start a new one';
export const RECOVERY_START_NEW_TITLE = 'Start a new recovery key?';
export const RECOVERY_START_NEW_BODY = 'Recordings made under the old key stay locked until their recovery key is entered. Recording continues under the new key.';
export const RECOVERY_START_NEW_CONFIRM = 'Start a new key';
export const RECOVERY_START_NEW_CANCEL = 'Keep looking';
export const RECOVERY_START_NEW_FAILED = 'A new recovery key could not be started.';

// Library, while the content key is not open. Recording and Settings keep
// working.
export const LOCKED_LIBRARY_TITLE = 'Your recordings are locked';
export const LOCKED_LIBRARY_BODY = 'You can keep recording. Enter your recovery key to open your recordings.';
export const WAITING_LIBRARY_TITLE = 'Waiting for your other desktop';
export const WAITING_LIBRARY_BODY = 'This profile\'s encryption key has not synced in yet. You can keep recording; your recordings open once it arrives.';
export const LOCKED_LIBRARY_ENTER = 'Enter recovery key';

// Library rows whose content key is not open: only the date is readable.
export const LOCKED_ROW_TITLE = 'Locked recording';
export const LOCKED_ROW_ENTER = 'Enter old recovery key';
// A locked row whose key has not synced in from another desktop yet.
export const WAITING_ROW_TITLE = 'Waiting for your other desktop';
export const WAITING_ROW_HINT = 'This recording opens once its key syncs in.';

// Notes tab, when the notes file does not open.
export const NOTES_UNREADABLE = 'These notes could not be opened.';
