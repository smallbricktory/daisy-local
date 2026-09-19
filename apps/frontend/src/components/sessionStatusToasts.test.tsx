// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render } from '@testing-library/react';
import type { Phase } from '../lib/sessionPhase';

let phase: Phase = { kind: 'idle' };
const dismissPhase = vi.fn();
const beginFinalizeWatch = vi.fn();
vi.mock('../lib/sessionLifecycle', () => ({
  useSessionPhase: () => phase,
  dismissPhase: () => dismissPhase(),
  beginFinalizeWatch: (...a: unknown[]) => beginFinalizeWatch(...a),
}));

const resumeFinalize = vi.fn();
const runFinalize = vi.fn();
vi.mock('../lib/finalizeRunner', () => ({
  resumeFinalize: (id: string) => { resumeFinalize(id); return Promise.resolve(); },
  runFinalize: (...a: unknown[]) => { runFinalize(...a); return Promise.resolve(); },
}));

let aiStatus = { configured: false, state: 'Missing', provider: null as string | null, hint: null as string | null };
vi.mock('../lib/aiProviderStatus', () => ({
  useAiProviderStatus: () => aiStatus,
}));

const pushToast = vi.fn();
const dismissToast = vi.fn();
vi.mock('../lib/toastStore', () => ({
  pushToast: (s: unknown) => pushToast(s),
  dismissToast: (id: string) => dismissToast(id),
}));

const openLabelModal = vi.fn();
vi.mock('../lib/labelModalStore', () => ({
  openLabelModal: (...a: unknown[]) => openLabelModal(...a),
}));

import { SessionStatusToasts, SESSION_STATUS_TOAST_ID } from './sessionStatusToasts';

const actions = {
  onOpenRecording: vi.fn(), onOpenSession: vi.fn(), onResume: vi.fn(),
  onCopyPrompt: vi.fn(), onWantAi: vi.fn(),
};

beforeEach(() => {
  pushToast.mockClear(); dismissToast.mockClear(); openLabelModal.mockClear();
  dismissPhase.mockClear(); beginFinalizeWatch.mockClear(); resumeFinalize.mockClear();
  runFinalize.mockClear();
  aiStatus = { configured: false, state: 'Missing', provider: null, hint: null };
  Object.values(actions).forEach((f) => f.mockClear());
});

interface ToastAction { label: string; onClick: () => void }
interface CapturedToast { severity: string; title: string; progress?: number; actions?: ToastAction[] }
function lastToast(): CapturedToast { return pushToast.mock.calls.at(-1)?.[0] as CapturedToast; }

describe('SessionStatusToasts', () => {
  it('idle dismisses the session-status toast', () => {
    phase = { kind: 'idle' };
    render(<SessionStatusToasts {...actions} />);
    expect(dismissToast).toHaveBeenCalledWith(SESSION_STATUS_TOAST_ID);
  });

  it('finalizing pushes a working toast carrying progress', () => {
    phase = { kind: 'finalizing', sessionId: 's', title: 'Mtg', stage: 'transcribing', progress: 0.4 };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.severity).toBe('working');
    expect(t.progress).toBe(0.4);
    expect(t.title).toContain('Mtg');
  });

  it('needs-labels warning toast opens the label modal on action (no auto-pop)', () => {
    phase = { kind: 'needs-labels', sessionId: 's7', title: 'Mtg', clusters: [1, 2] };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.severity).toBe('warning');
    t.actions!.find((a) => a.label === 'Label speakers')!.onClick();
    expect(openLabelModal).toHaveBeenCalledWith('s7', 'Mtg');
  });

  it('needs-labels "Later" still resumes finalize so the session gets summarized', () => {
    phase = { kind: 'needs-labels', sessionId: 's8', title: 'Mtg', clusters: [1] };
    render(<SessionStatusToasts {...actions} />);
    lastToast().actions!.find((a) => a.label === 'Later')!.onClick();
    expect(resumeFinalize).toHaveBeenCalledWith('s8');
    expect(beginFinalizeWatch).toHaveBeenCalled();
    expect(dismissPhase).toHaveBeenCalled();
  });

  it('needs-labels X works like "Later", so the toast does not come back', () => {
    phase = { kind: 'needs-labels', sessionId: 's9', title: 'Mtg', clusters: [1] };
    render(<SessionStatusToasts {...actions} />);
    (lastToast() as CapturedToast & { onDismiss?: () => void }).onDismiss!();
    expect(resumeFinalize).toHaveBeenCalledWith('s9');
    expect(beginFinalizeWatch).toHaveBeenCalled();
    expect(dismissPhase).toHaveBeenCalled();
  });

  it('done with no summary and no provider offers Open + Copy prompt + Want AI', () => {
    phase = { kind: 'done', sessionId: 's', title: 'Mtg',
      summary: { hadAi: false, speakers: null, durationLabel: null } };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.severity).toBe('done');
    expect(t.actions!.map((a) => a.label)).toEqual(['Open', 'Copy prompt', 'Want AI? →']);
  });

  it('done with no summary but a configured provider offers Generate summary', () => {
    aiStatus = { configured: true, state: 'Configured', provider: 'daisy-cloud', hint: null };
    phase = { kind: 'done', sessionId: 's9', title: 'Mtg',
      summary: { hadAi: false, speakers: null, durationLabel: null } };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.actions!.map((a) => a.label)).toEqual(['Open', 'Generate summary']);
    t.actions!.find((a) => a.label === 'Generate summary')!.onClick();
    expect(runFinalize).toHaveBeenCalledWith('s9', 'regen-summary', true);
  });

  it('done with a locked vault names the vault instead of selling AI', () => {
    aiStatus = { configured: false, state: 'VaultLocked', provider: null, hint: null };
    phase = { kind: 'done', sessionId: 's10', title: 'Mtg',
      summary: { hadAi: false, speakers: null, durationLabel: null } };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.actions!.map((a) => a.label)).toEqual(['Open']);
  });

  it('interrupted offers Resume wired to onResume', () => {
    phase = { kind: 'interrupted', sessionId: 's', title: 'Mtg', lastStage: 'summarizing' };
    render(<SessionStatusToasts {...actions} />);
    const t = lastToast();
    expect(t.severity).toBe('error');
    t.actions!.find((a) => a.label === 'Resume')!.onClick();
    expect(actions.onResume).toHaveBeenCalledWith('s', 'Mtg');
  });
});
