// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One path for finishing the active recording from outside the record screen
// (the mini window's Stop, the meeting-ended reminder's Stop). The backend
// emits `daisy://recording/finish` to the main window, which runs
// `finishActiveRecording`: the mounted record screen finishes itself
// (flushing its notes first); otherwise the session is handed straight to the
// finalize cascade.

let screenFinisher: (() => void) | null = null;

/** Registers the mounted record screen's Finish. Returns the unregister. */
export function registerScreenFinisher(fn: () => void): () => void {
  screenFinisher = fn;
  return () => { if (screenFinisher === fn) screenFinisher = null; };
}

/** Finishes the recording `sessionId` through the record screen when one is
 *  mounted, else through `startCascade`. */
export function finishActiveRecording(
  sessionId: string,
  startCascade: (sessionId: string, title: string) => void,
): void {
  if (screenFinisher) screenFinisher();
  else startCascade(sessionId, sessionId);
}
