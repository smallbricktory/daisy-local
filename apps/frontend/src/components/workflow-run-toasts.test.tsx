import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, act } from '@testing-library/react';
import type { WorkflowRunEvent } from '../tauri';

const pushToast = vi.hoisted(() => vi.fn());
vi.mock('../lib/toastStore', () => ({ pushToast }));

type Handler = (e: { payload: WorkflowRunEvent }) => void;
const handlers = vi.hoisted(() => ({ current: null as Handler | null }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn((_name: string, cb: Handler) => {
    handlers.current = cb;
    return Promise.resolve(() => {});
  }),
}));

import { WorkflowRunToasts } from './workflowRunToasts';

const ev = (over: Partial<WorkflowRunEvent>): WorkflowRunEvent => ({
  run_id: 'r1',
  workflow_name: 'Design specs',
  session_id: 's1',
  step_index: 1,
  step_count: 3,
  step_label: 'Run prompt: PM',
  status: 'running',
  ...over,
});

const emit = (e: WorkflowRunEvent) => act(() => { handlers.current!({ payload: e }); });

beforeEach(() => {
  vi.clearAllMocks();
  handlers.current = null;
});

describe('WorkflowRunToasts', () => {
  it('running event shows a working toast with step progress', async () => {
    render(<WorkflowRunToasts onOpenHistory={() => {}} />);
    await act(async () => {}); // flush listen()
    emit(ev({}));
    expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({
      id: 'workflow:r1',
      severity: 'working',
      title: 'Design specs',
      body: expect.stringContaining('Run prompt: PM'),
      progress: 1 / 3,
    }));
  });

  it('terminal ok flips to done with auto-dismiss', async () => {
    render(<WorkflowRunToasts onOpenHistory={() => {}} />);
    await act(async () => {});
    emit(ev({ step_index: 3, step_label: '', status: 'ok' }));
    expect(pushToast).toHaveBeenCalledWith(expect.objectContaining({
      id: 'workflow:r1',
      severity: 'done',
      autoDismissMs: 8000,
    }));
  });

  it('error is sticky and clicks through to history', async () => {
    const onOpenHistory = vi.fn();
    render(<WorkflowRunToasts onOpenHistory={onOpenHistory} />);
    await act(async () => {});
    emit(ev({ run_id: 'r2', status: 'error' }));
    const call = pushToast.mock.calls.find((c) => c[0].id === 'workflow:r2')![0];
    expect(call.severity).toBe('error');
    expect(call.autoDismissMs).toBeUndefined();
    call.onClick();
    expect(onOpenHistory).toHaveBeenCalled();
  });

  it('partial maps to warning', async () => {
    render(<WorkflowRunToasts onOpenHistory={() => {}} />);
    await act(async () => {});
    emit(ev({ run_id: 'r3', status: 'partial' }));
    const call = pushToast.mock.calls.find((c) => c[0].id === 'workflow:r3')![0];
    expect(call.severity).toBe('warning');
  });
});
