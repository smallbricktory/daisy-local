// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  IMPORT_STALL_MS, clearImportStall, dropImportSession, importStageLine, syncImportToast,
  syncImportToastId, watchImportStall, __resetImportStallsForTests,
} from './syncImport';
import { pushToast } from './toastStore';
import type { SyncImport, SyncImportStage } from '../tauri';

vi.mock('./toastStore', () => ({ pushToast: vi.fn() }));

function ev(over: Partial<SyncImport> = {}): SyncImport {
  return {
    device_id: 'a1', device_name: "Rowan's phone", session: 'daisy-1787249858',
    stage: 'receiving', received: null, total: null, title: null, library_session: null, ...over,
  };
}

/** The toast an event leaves, which every case here expects to exist. */
function toastFor(e: SyncImport, onOpen: (id: string) => void = () => {}) {
  const t = syncImportToast(e, onOpen);
  expect(t).not.toBeNull();
  return t!;
}

beforeEach(() => { __resetImportStallsForTests(); });

describe('one import toast per device', () => {
  it('every stage replaces one toast, so an import never stacks up', () => {
    const stages: SyncImportStage[] =
      ['receiving', 'importing', 'audio', 'transcript', 'summary', 'chapters', 'done'];
    const ids = stages.map((stage) => toastFor(ev({ stage })).id);
    expect(new Set(ids).size).toBe(1);
    expect(ids[0]).toBe(syncImportToastId('a1'));
  });

  it('stays up while the work runs and names the phone it came from', () => {
    const t = toastFor(ev({ stage: 'summary' }));
    expect(t.severity).toBe('working');
    expect(t.autoDismissMs).toBeUndefined();
    expect(t.title).toContain("Rowan's phone");
    expect(t.body).toBe('Writing the summary');
  });

  it('counts the audio as it lands, and shows a bar only once it can', () => {
    const counted = toastFor(ev({ received: 3, total: 12 }));
    expect(counted.body).toBe('Receiving audio — 3 of 12 pieces');
    expect(counted.progress).toBeCloseTo(0.25);
    __resetImportStallsForTests();
    const uncounted = toastFor(ev());
    expect(uncounted.body).toBe('Receiving files');
    expect(uncounted.progress).toBeUndefined();
  });

  it('the last stage names the meeting, opens it, and goes away on its own', () => {
    const onOpen = vi.fn();
    const t = toastFor(ev({ stage: 'done', title: 'Harbour budget' }), onOpen);
    expect(t.severity).toBe('done');
    expect(t.title).toContain('Harbour budget');
    expect(t.autoDismissMs).toBeGreaterThan(0);
    t.onClick?.();
    expect(onOpen).toHaveBeenCalledWith('daisy-1787249858');
  });

  it('opens the meeting under the id this library holds it by', () => {
    const onOpen = vi.fn();
    const t = toastFor(
      ev({ stage: 'done', title: 'Harbour budget', library_session: 'daisy-900000000000' }),
      onOpen,
    );
    t.onClick?.();
    expect(onOpen).toHaveBeenCalledWith('daisy-900000000000');
  });

  it('an untitled meeting and an unnamed device still read as sentences', () => {
    const t = toastFor(ev({ stage: 'done', title: '  ', device_name: ' ' }));
    expect(t.title).toBe('A recording arrived from your device');
  });

  it('every stage has words of its own', () => {
    const stages: SyncImportStage[] =
      ['receiving', 'importing', 'audio', 'transcript', 'summary', 'chapters'];
    const lines = stages.map((stage) => importStageLine(ev({ stage })));
    expect(new Set(lines).size).toBe(stages.length);
    expect(lines.every((l) => l.length > 0)).toBe(true);
  });
});

describe('a device sending several recordings', () => {
  const three = ['daisy-1', 'daisy-2', 'daisy-3'];

  it('counts them in one toast instead of one toast each', () => {
    const ids = three.map((session) => toastFor(ev({ session })).id);
    expect(new Set(ids).size).toBe(1);
    const t = toastFor(ev({ session: 'daisy-3', stage: 'summary' }));
    expect(t.title).toBe("Importing 3 recordings from Rowan's phone");
    expect(t.body).toBe('0 of 3 done · Writing the summary');
    expect(t.progress).toBe(0);
  });

  it('the count moves as each one lands, and the toast stays up until the last', () => {
    three.forEach((session) => toastFor(ev({ session })));
    const first = toastFor(ev({ session: 'daisy-1', stage: 'done', title: 'One' }));
    expect(first.severity).toBe('working');
    expect(first.body).toContain('1 of 3 done');
    expect(first.progress).toBeCloseTo(1 / 3);
    const second = toastFor(ev({ session: 'daisy-2', stage: 'done', title: 'Two' }));
    expect(second.severity).toBe('working');
    expect(second.body).toContain('2 of 3 done');
  });

  it('one toast says they arrived, and opens the last of them', () => {
    const onOpen = vi.fn();
    three.forEach((session) => toastFor(ev({ session }), onOpen));
    let last = toastFor(ev({ session: 'daisy-1', stage: 'done', title: 'One' }), onOpen);
    last = toastFor(ev({ session: 'daisy-2', stage: 'done', title: 'Two' }), onOpen);
    last = toastFor(
      ev({ session: 'daisy-3', stage: 'done', title: 'Three', library_session: 'daisy-9' }),
      onOpen,
    );
    expect(last.severity).toBe('done');
    expect(last.title).toBe("3 recordings arrived from Rowan's phone");
    expect(last.autoDismissMs).toBeGreaterThan(0);
    last.onClick?.();
    expect(onOpen).toHaveBeenCalledWith('daisy-9');
  });

  it('a session the import hands back is counted as going again', () => {
    toastFor(ev({ session: 'daisy-1' }));
    const done = toastFor(ev({ session: 'daisy-1', stage: 'done', title: 'One' }));
    expect(done.severity).toBe('done');
    const again = toastFor(ev({ session: 'daisy-1', stage: 'receiving', received: 2, total: 7 }));
    expect(again.severity).toBe('working');
    expect(again.body).toBe('Receiving audio — 2 of 7 pieces');
    const landed = toastFor(ev({ session: 'daisy-1', stage: 'done', title: 'One' }));
    expect(landed.severity).toBe('done');
  });

  it('the same events delivered twice count once', () => {
    const onOpen = vi.fn();
    const twice = (e: SyncImport) => { toastFor(e, onOpen); return toastFor(e, onOpen); };
    three.forEach((session) => twice(ev({ session })));
    twice(ev({ session: 'daisy-1', stage: 'done', title: 'One' }));
    twice(ev({ session: 'daisy-2', stage: 'done', title: 'Two' }));
    const last = twice(ev({ session: 'daisy-3', stage: 'done', title: 'Three' }));
    expect(last.severity).toBe('done');
    expect(last.title).toBe("3 recordings arrived from Rowan's phone");
  });

  it('what the device sends after a batch landed starts a new one', () => {
    toastFor(ev({ session: 'daisy-1' }));
    toastFor(ev({ session: 'daisy-1', stage: 'done', title: 'One' }));
    const next = toastFor(ev({ session: 'daisy-2', stage: 'receiving' }));
    expect(next.severity).toBe('working');
    expect(next.title).toBe("Importing a recording from Rowan's phone");
  });

  it('a refused session leaves the others counted', () => {
    three.forEach((session) => toastFor(ev({ session })));
    const left = dropImportSession('a1', 'daisy-2', () => {});
    expect(left?.severity).toBe('working');
    expect(left?.title).toBe("Importing 2 recordings from Rowan's phone");
  });

  it('the last session refused leaves nothing to show', () => {
    toastFor(ev({ session: 'daisy-1' }));
    expect(dropImportSession('a1', 'daisy-1', () => {})).toBeNull();
  });

  it('two devices sending at once keep a toast each', () => {
    const a = toastFor(ev({ device_id: 'a1', device_name: "Rowan's phone" }));
    const b = toastFor(ev({ device_id: 'b2', device_name: "Rowan's tablet" }));
    expect(a.id).not.toBe(b.id);
    expect(b.title).toContain("Rowan's tablet");
  });
});

describe('import stall watch', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    __resetImportStallsForTests();
    vi.mocked(pushToast).mockClear();
  });
  afterEach(() => { vi.useRealTimers(); });

  it('a device that stops sending files says it is being waited on', () => {
    const e = ev({ stage: 'receiving', received: 2, total: 7 });
    toastFor(e);
    watchImportStall(e);
    vi.advanceTimersByTime(IMPORT_STALL_MS);
    expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({
      id: syncImportToastId('a1'),
      severity: 'info',
      title: "Waiting for Rowan's phone",
    }));
  });

  it('more files arriving put the wait off again', () => {
    watchImportStall(ev({ stage: 'receiving', received: 2, total: 7 }));
    vi.advanceTimersByTime(IMPORT_STALL_MS - 1000);
    watchImportStall(ev({ stage: 'receiving', received: 3, total: 7 }));
    vi.advanceTimersByTime(IMPORT_STALL_MS - 1000);
    expect(pushToast).not.toHaveBeenCalled();
  });

  it('a stage this computer runs is never called waiting', () => {
    const mine: SyncImportStage[] = ['importing', 'audio', 'transcript', 'summary', 'chapters'];
    for (const stage of mine) {
      watchImportStall(ev({ stage }));
      vi.advanceTimersByTime(IMPORT_STALL_MS * 2);
    }
    expect(pushToast).not.toHaveBeenCalled();
  });

  it('a session this computer is working on holds the wait off for the device', () => {
    toastFor(ev({ session: 'daisy-1', stage: 'receiving' }));
    toastFor(ev({ session: 'daisy-2', stage: 'summary' }));
    watchImportStall(ev({ session: 'daisy-1', stage: 'receiving' }));
    vi.advanceTimersByTime(IMPORT_STALL_MS);
    expect(pushToast).not.toHaveBeenCalled();
  });

  it('two devices holding one session id keep their own toast and their own wait', () => {
    const a = ev({ stage: 'receiving', device_id: 'a1', device_name: "Rowan's phone" });
    const b = ev({ stage: 'receiving', device_id: 'b2', device_name: "Rowan's tablet" });
    watchImportStall(a);
    watchImportStall(b);
    expect(syncImportToastId('a1')).not.toBe(syncImportToastId('b2'));
    vi.advanceTimersByTime(IMPORT_STALL_MS);
    expect(pushToast).toHaveBeenCalledTimes(2);
    expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({
      title: "Waiting for Rowan's phone",
    }));
    expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({
      title: "Waiting for Rowan's tablet",
    }));
  });

  it('a device whose last session is refused stops being waited on', () => {
    toastFor(ev({ stage: 'receiving' }));
    watchImportStall(ev({ stage: 'receiving' }));
    dropImportSession('a1', 'daisy-1787249858', () => {});
    vi.advanceTimersByTime(IMPORT_STALL_MS * 2);
    expect(pushToast).not.toHaveBeenCalled();
  });

  it('a device taken off watch by hand is not waited on', () => {
    watchImportStall(ev({ stage: 'receiving' }));
    clearImportStall('a1');
    vi.advanceTimersByTime(IMPORT_STALL_MS * 2);
    expect(pushToast).not.toHaveBeenCalled();
  });
});
