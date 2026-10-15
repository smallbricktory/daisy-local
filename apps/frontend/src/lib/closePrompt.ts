// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The first-close question: closing the main window minimizes Daisy or
// quits it. The answer goes to the backend, which remembers it.
import { tauri } from '../tauri';
import { choose } from './confirm';

export async function askCloseChoice(): Promise<void> {
  const result = await choose({
    title: 'Minimize Daisy when you close the window?',
    body: 'Daisy keeps running and recording in the taskbar or Dock. Change this in Settings \u2192 Display, with Nerd Mode on.',
    confirmLabel: 'Minimize',
    alternateLabel: 'Quit',
    cancelLabel: '__hidden__',
  });
  await tauri.resolveClosePrompt(
    result === 'confirm' ? 'minimize' : result === 'alternate' ? 'quit' : 'cancel',
  );
}
