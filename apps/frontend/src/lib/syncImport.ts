// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// The words for a phone's recordings on their way in, and the toast the
// `sync:import` event keeps up while they travel.
//
// One toast per device, for everything that device is sending.

import type { SyncImport, SyncImportStage } from '../tauri';
import { pushToast, type ToastSpec } from './toastStore';

/** One toast per device, replaced in place as its sessions advance. */
export function syncImportToastId(device: string): string {
  return `sync-import-${device}`;
}

/** How long a device waits for more files before the toast stops claiming
 *  they are arriving. */
export const IMPORT_STALL_MS = 5 * 60 * 1000;

/** A session whose files are still arriving or whose pipeline is running. */
interface Active {
  stage: SyncImportStage;
  received: number | null;
  total: number | null;
}

/** A session that reached the library. */
interface Arrived {
  session: string;
  library_session: string | null;
  title: string | null;
}

/** What one device is sending: the sessions still going, the ones that
 *  landed, and the session its last event named. */
interface Batch {
  name: string;
  active: Map<string, Active>;
  arrived: Arrived[];
  latest: string;
}

const batches = new Map<string, Batch>();
const stalls = new Map<string, ReturnType<typeof setTimeout>>();

function batchOf(ev: SyncImport): Batch {
  const held = batches.get(ev.device_id);
  if (held) {
    held.name = ev.device_name;
    return held;
  }
  const fresh: Batch = { name: ev.device_name, active: new Map(), arrived: [], latest: ev.session };
  batches.set(ev.device_id, fresh);
  return fresh;
}

/** The device as a person names it, falling back to the plain noun. */
function phone(name: string): string {
  return name.trim() || 'your device';
}

/** True when every chunk the manifest names is here: what a session still
 *  receiving waits on is something other than its audio. */
function hasAllAudio(a: Pick<SyncImport, 'received' | 'total'>): boolean {
  return a.total != null && a.received != null && a.total > 0 && a.received >= a.total;
}

/** What is happening right now, as the toast's second line. */
export function importStageLine(ev: Pick<SyncImport, 'stage' | 'received' | 'total'>): string {
  const stage: SyncImportStage = ev.stage;
  if (stage === 'receiving') {
    if (ev.total == null || ev.received == null) return 'Receiving files';
    return hasAllAudio(ev)
      ? 'All audio received — waiting for the rest'
      : `Receiving audio — ${ev.received} of ${ev.total} pieces`;
  }
  if (stage === 'importing') return 'Adding it to your library';
  if (stage === 'audio') return 'Building the playback audio';
  if (stage === 'transcript') return 'Writing the transcript';
  if (stage === 'summary') return 'Writing the summary';
  if (stage === 'chapters') return 'Writing chapters';
  return 'Ready';
}

/** The session the toast speaks for: the one the last event named while it is
 *  still going, and otherwise the one that has been going longest. */
function spokenFor(batch: Batch): [string, Active] | undefined {
  const latest = batch.active.get(batch.latest);
  if (latest) return [batch.latest, latest];
  return batch.active.entries().next().value as [string, Active] | undefined;
}

/** The toast for what a device is sending, or `null` once it is sending
 *  nothing: everything it sent has arrived and has its own toast. */
function batchToast(
  device: string,
  batch: Batch,
  onOpen: (sessionId: string) => void,
): ToastSpec | null {
  if (batch.active.size > 0) return workingToast(device, batch);

  clearImportStall(device);
  const id = syncImportToastId(device);
  const who = phone(batch.name);
  const last = batch.arrived[batch.arrived.length - 1];
  if (last === undefined) return null;
  if (batch.arrived.length === 1) {
    const title = last.title?.trim();
    return {
      id,
      severity: 'done',
      title: title ? `"${title}" arrived from ${who}` : `A recording arrived from ${who}`,
      onClick: () => onOpen(last.library_session ?? last.session),
      autoDismissMs: 8000,
    };
  }
  return {
    id,
    severity: 'done',
    title: `${batch.arrived.length} recordings arrived from ${who}`,
    onClick: () => onOpen(last.library_session ?? last.session),
    autoDismissMs: 8000,
  };
}

/** The working toast for a device with sessions still going. */
function workingToast(device: string, batch: Batch): ToastSpec {
  const id = syncImportToastId(device);
  const who = phone(batch.name);
  const all = batch.active.size + batch.arrived.length;
  const spoken = spokenFor(batch);
  const line = spoken ? importStageLine(spoken[1]) : 'Receiving files';
  if (all === 1) {
    const one = spoken?.[1];
    const progress =
      one && one.stage === 'receiving' && one.total != null && one.received != null && one.total > 0
        ? one.received / one.total
        : undefined;
    return {
      id,
      severity: 'working',
      title: `Importing a recording from ${who}`,
      body: line,
      progress,
    };
  }
  return {
    id,
    severity: 'working',
    title: `Importing ${all} recordings from ${who}`,
    body: `${batch.arrived.length} of ${all} done · ${line}`,
    progress: batch.arrived.length / all,
  };
}

/**
 * The toast for one `sync:import` event. Every stage but `done` keeps a
 * working toast up for the device; `done` on the last session it is sending
 * replaces that with a dismissible one that opens what arrived.
 */
export function syncImportToast(
  ev: SyncImport,
  onOpen: (sessionId: string) => void,
): ToastSpec | null {
  const batch = batchOf(ev);
  batch.latest = ev.session;
  // Everything the device sent has arrived, and now it sends something else:
  // that is a batch of its own. A session already counted, or the same event
  // delivered twice, stays in the batch it landed in.
  if (batch.active.size === 0 && batch.arrived.length > 0
      && !(ev.stage === 'done' || batch.arrived.some((a) => a.session === ev.session))) {
    batch.arrived = [];
  }
  if (ev.stage === 'done') {
    batch.active.delete(ev.session);
    batch.arrived = batch.arrived.filter((a) => a.session !== ev.session);
    batch.arrived.push({
      session: ev.session,
      library_session: ev.library_session,
      title: ev.title,
    });
  } else {
    // A stage after `done` — the import gave the session back to the phone
    // — puts it back among the ones still going.
    batch.arrived = batch.arrived.filter((a) => a.session !== ev.session);
    batch.active.set(ev.session, { stage: ev.stage, received: ev.received, total: ev.total });
  }
  return batchToast(ev.device_id, batch, onOpen);
}

/**
 * Takes a session out of what a device is sending: it was refused. Returns
 * the toast the device is left with, or `null` when nothing of it is left to
 * show.
 */
export function dropImportSession(
  device: string,
  session: string,
  onOpen: (sessionId: string) => void,
): ToastSpec | null {
  const batch = batches.get(device);
  if (batch === undefined) return null;
  batch.active.delete(session);
  batch.arrived = batch.arrived.filter((a) => a.session !== session);
  if (batch.active.size === 0 && batch.arrived.length === 0) {
    batches.delete(device);
    clearImportStall(device);
    return null;
  }
  return batchToast(device, batch, onOpen);
}

/** True while a device's session is still on its way in: its files are
 *  arriving or the pipeline is running on it. */
export function importStillGoing(device: string, session: string): boolean {
  return batches.get(device)?.active.has(session) ?? false;
}

/** Takes a device off stall watch. */
export function clearImportStall(device: string): void {
  const timer = stalls.get(device);
  if (timer !== undefined) clearTimeout(timer);
  stalls.delete(device);
}

/**
 * Puts a device on stall watch while its files are still arriving. Only a
 * file landing (`receiving`) starts the wait over.
 */
export function watchImportStall(ev: SyncImport): void {
  if (ev.stage !== 'receiving') return;
  clearImportStall(ev.device_id);
  stalls.set(ev.device_id, setTimeout(() => {
    stalls.delete(ev.device_id);
    importStalled(ev.device_id, ev.device_name);
  }, IMPORT_STALL_MS));
}

/**
 * No file has come from a device for `IMPORT_STALL_MS`. Its sessions still
 * receiving leave the batch. A session this computer is working on keeps the
 * toast and says what it is doing; with none left, the toast says the device
 * is being waited on.
 */
function importStalled(device: string, name: string): void {
  const batch = batches.get(device);
  const waiting = batch ? [...batch.active.values()].filter((a) => a.stage === 'receiving') : [];
  if (batch) {
    if (waiting.length === 0) return;
    for (const [session, a] of batch.active) if (a.stage === 'receiving') batch.active.delete(session);
    if (batch.active.size > 0) {
      pushToast(workingToast(device, batch));
      return;
    }
  }
  // What already arrived stays, so a late recording joins its count.
  if (!batch || batch.arrived.length === 0) batches.delete(device);
  const audioHere = waiting.length > 0 && waiting.every(hasAllAudio);
  pushToast({
    id: syncImportToastId(device),
    severity: 'info',
    title: `Waiting for ${phone(batch?.name ?? name)}`,
    body: audioHere
      ? 'All the audio is here. Daisy adds the recording when the rest arrives.'
      : 'Daisy picks the recording up again when the device is back.',
    autoDismissMs: 10000,
  });
}

/** Takes every device off stall watch: nothing is listening any more. */
export function clearAllImportStalls(): void {
  for (const timer of stalls.values()) clearTimeout(timer);
  stalls.clear();
}

/** Drops every batch and stall watch. Tests only. */
export function __resetImportStallsForTests(): void {
  clearAllImportStalls();
  batches.clear();
}
