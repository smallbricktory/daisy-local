import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

vi.mock('../tauri', () => ({
  tauri: {
    integrationHistory: vi.fn(),
    workflowHistory: vi.fn(),
    workflowQueueState: vi.fn(),
  },
  errStr: vi.fn((e: unknown) => String(e)),
}));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import { tauri } from '../tauri';
import { History } from './History';

const mock = tauri as unknown as Record<string, ReturnType<typeof vi.fn>>;

beforeEach(() => {
  vi.clearAllMocks();
  mock.integrationHistory.mockResolvedValue([]);
  mock.workflowHistory.mockResolvedValue([]);
  mock.workflowQueueState.mockResolvedValue([]);
});

describe('History merged view', () => {
  it('merges manual sends and workflow runs by timestamp', async () => {
    mock.integrationHistory.mockResolvedValue([
      { at_unix_seconds: 200, session_id: 's1', meeting_id: 'm1', meeting_title: 'Acme kickoff',
        integration_id: 'i1', integration_name: 'Ops hook', kind: 'webhook', payloads_sent: ['summary'], status: 'ok' },
    ]);
    mock.workflowHistory.mockResolvedValue([
      { run_id: 'r1', at_unix_seconds: 300, workflow_id: 'w1', workflow_name: 'Design specs',
        session_id: 's2', session_title: 'Retro', trigger: 'finalized', status: 'ok',
        steps: [{ label: 'Run prompt: PM', status: 'ok', duration_ms: 900 }] },
    ]);
    render(<History onOpenSession={() => {}} />);
    expect(await screen.findByText('Design specs')).toBeInTheDocument();
    const rows = screen.getAllByRole('row');
    const text = rows.map((r) => r.textContent).join('|');
    // workflow run (t=300) sorts above manual send (t=200)
    expect(text.indexOf('Design specs')).toBeLessThan(text.indexOf('Ops hook'));
  });

  it('expands a workflow run to show steps', async () => {
    mock.workflowHistory.mockResolvedValue([
      { run_id: 'r1', at_unix_seconds: 300, workflow_id: 'w1', workflow_name: 'Design specs',
        session_id: 's2', session_title: 'Retro', trigger: 'finalized', status: 'partial',
        steps: [
          { label: 'Run prompt: PM', status: 'ok', duration_ms: 900 },
          { label: 'Send to Ops hook', status: 'error: webhook returned 500', duration_ms: 40 },
        ] },
    ]);
    render(<History onOpenSession={() => {}} />);
    fireEvent.click(await screen.findByRole('button', { name: /expand run/i }));
    expect(await screen.findByText('Run prompt: PM')).toBeInTheDocument();
    expect(screen.getByText(/webhook returned 500/)).toBeInTheDocument();
  });

  it('shows queued runs as in-flight', async () => {
    mock.workflowQueueState.mockResolvedValue([
      { run_id: 'r9', workflow_id: 'w1', workflow_name: 'Design specs', session_id: 's3',
        session_title: 'Standup', tag_ids: [], trigger: 'finalized', actions: [], attempts: 0,
        created_at_unix_seconds: 400 },
    ]);
    render(<History onOpenSession={() => {}} />);
    expect(await screen.findByText(/queued/i)).toBeInTheDocument();
    expect(screen.getByText('Design specs')).toBeInTheDocument();
  });

  it('empty state mentions both sources', async () => {
    render(<History onOpenSession={() => {}} />);
    expect(await screen.findByText(/Workflow runs and manual sends/i)).toBeInTheDocument();
  });
});
