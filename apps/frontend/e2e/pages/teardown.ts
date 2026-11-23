// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The shared teardown loop: rows a spec created outlive its file, so a
// `finally` clears every row carrying the name. Idempotent, and copes with a
// dialog or editor left open by a failed case.
import { confirmDialog } from './shell.ts';

/** How many rows one teardown loop will clear before it gives up. A backend
 *  that refuses every delete would otherwise spin to the mocha timeout, which
 *  reports as a hang rather than as the failure it is. */
export const TEARDOWN_MAX_ROUNDS = 5;

export async function ensureAbsent(opts: {
  what: string;
  name: string;
  /** Routes to the list; also closes any editor the last case left open. */
  open: () => Promise<void>;
  idByName: (name: string) => Promise<string | null>;
  /** Deletes one row and waits for it to leave the list. */
  deleteOne: (id: string) => Promise<void>;
}): Promise<void> {
  const { what, name, open, idByName, deleteOne } = opts;
  if (await confirmDialog.modal().isExisting()) await confirmDialog.cancelOut();
  await open();
  for (let round = 0; round < TEARDOWN_MAX_ROUNDS; round++) {
    const id = await idByName(name);
    if (id === null) return;
    await deleteOne(id);
  }
  if ((await idByName(name)) !== null) {
    throw new Error(
      `teardown gave up after ${TEARDOWN_MAX_ROUNDS} rounds: ${what} "${name}" is still listed`,
    );
  }
}
