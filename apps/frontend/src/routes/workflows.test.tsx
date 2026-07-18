import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import type { Workflow } from '../tauri';

vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn().mockResolvedValue('/tmp/out') }));

vi.mock('../tauri', () => ({
  tauri: {
    workflowsList: vi.fn(),
    workflowUpsert: vi.fn(),
    workflowDelete: vi.fn(),
    listTags: vi.fn(),
    searchTags: vi.fn(),
    listContacts: vi.fn(),
    listPrompts: vi.fn(),
    listIntegrations: vi.fn(),
  },
  errStr: vi.fn((e: unknown) => String(e)),
}));
vi.mock('../lib/confirm', () => ({ confirm: vi.fn().mockResolvedValue(true) }));
// The combobox drives its own tauri calls; stub it with a one-click picker.
vi.mock('../components/tags/TagCombobox', () => ({
  TagCombobox: ({ onPick }: { onPick: (t: { id: string; name: string; color_hex: string }) => void }) => (
    <button onClick={() => onPick({ id: 't1', name: 'Client A', color_hex: '#aabbcc' })}>pick-tag</button>
  ),
}));

import { tauri } from '../tauri';
import { Workflows } from './Workflows';

const mock = tauri as unknown as Record<string, ReturnType<typeof vi.fn>>;

const wf = (over: Partial<Workflow> = {}): Workflow => ({
  id: 'w1',
  name: 'Design specs',
  enabled: true,
  trigger: 'finalized',
  condition: { type: 'all', children: [] },
  actions: [{ type: 'push_integration', integration_id: 'i1' }],
  created_at_unix_seconds: 100,
  ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  mock.workflowsList.mockResolvedValue([wf(), wf({ id: 'w2', name: 'Archive hook', enabled: false })]);
  mock.workflowUpsert.mockResolvedValue(wf({ id: 'w3' }));
  mock.workflowDelete.mockResolvedValue(undefined);
  mock.listTags.mockResolvedValue([
    { id: 't1', name: 'Client A', color_hex: '#aabbcc', use_count: 1, created_at_unix_seconds: 0 },
  ]);
  mock.listContacts.mockResolvedValue([
    { id: 'c1', display_name: 'Joe Smith', emails: [], created_at_unix_seconds: 0 },
  ]);
  mock.listPrompts.mockResolvedValue([
    { id: 'builtin:pm', name: 'Project Manager coaching', output: 'sectioned', directive_md: 'x', builtin: true },
  ]);
  mock.listIntegrations.mockResolvedValue([
    { id: 'i1', name: 'Ops hook', enabled: true, kind: 'webhook', url: 'https://example.invalid', auth: 'none', auth_header_name: null, payloads: { summary: true, notes: false, transcript: false } },
  ]);
});

describe('Workflows list', () => {
  it('splits active and inactive', async () => {
    render(<Workflows />);
    expect(await screen.findByText('Design specs')).toBeInTheDocument();
    expect(screen.getByText('Active')).toBeInTheDocument();
    expect(screen.getByText('Inactive')).toBeInTheDocument();
    expect(screen.getByText('Archive hook')).toBeInTheDocument();
  });

  it('new workflow opens editor with defaults and saves', async () => {
    render(<Workflows />);
    fireEvent.click(await screen.findByText('New workflow'));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Fresh' } });
    // A save needs at least one action.
    fireEvent.click(screen.getByText('+ step'));
    fireEvent.click(screen.getByText('Send to integration'));
    fireEvent.click(screen.getByText('Save'));
    await waitFor(() => expect(mock.workflowUpsert).toHaveBeenCalled());
    const arg = mock.workflowUpsert.mock.calls[0][0] as Workflow;
    expect(arg.id).toBe('');
    expect(arg.name).toBe('Fresh');
    expect(arg.trigger).toBe('finalized');
    expect(arg.condition).toEqual({ type: 'all', children: [] });
    expect(arg.actions[0].type).toBe('push_integration');
  });
});

describe('Condition builder', () => {
  it('adds a tag condition and a nested group into the saved tree', async () => {
    render(<Workflows />);
    fireEvent.click(await screen.findByText('New workflow'));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Nested' } });
    // Tag leaf via the stubbed combobox.
    fireEvent.click(screen.getByText('+ condition'));
    fireEvent.click(screen.getByText('Tag'));
    fireEvent.click(screen.getByText('pick-tag'));
    // Nested group.
    fireEvent.click(screen.getByText('+ group'));
    fireEvent.click(screen.getByText('+ step'));
    fireEvent.click(screen.getByText('Send to integration'));
    fireEvent.click(screen.getByText('Save'));
    await waitFor(() => expect(mock.workflowUpsert).toHaveBeenCalled());
    const cond = (mock.workflowUpsert.mock.calls[0][0] as Workflow).condition;
    expect(cond.type).toBe('all');
    if (cond.type !== 'all') throw new Error('unreachable');
    expect(cond.children).toHaveLength(2);
    expect(cond.children[0]).toEqual({ type: 'has_tag', tag_id: 't1' });
    expect(['all', 'any']).toContain(cond.children[1].type);
  });
});

describe('Actions editor', () => {
  it('hides Run prompt for non-finalized triggers', async () => {
    render(<Workflows />);
    fireEvent.click(await screen.findByText('New workflow'));
    fireEvent.click(screen.getByText('+ step'));
    expect(screen.getByText('Run prompt')).toBeInTheDocument();
    // Toggle the menu closed (no step added), switch trigger, reopen.
    fireEvent.click(screen.getByText('+ step'));
    fireEvent.change(screen.getByLabelText('When'), { target: { value: 'deleted' } });
    fireEvent.click(screen.getByText('+ step'));
    expect(screen.getByText('Send to integration')).toBeInTheDocument();
    expect(screen.queryByText('Run prompt')).not.toBeInTheDocument();
  });

  it('blocks save when run-prompt steps exist on a non-finalized trigger', async () => {
    render(<Workflows />);
    fireEvent.click(await screen.findByText('New workflow'));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Bad' } });
    fireEvent.click(screen.getByText('+ step'));
    fireEvent.click(screen.getByText('Run prompt'));
    fireEvent.change(screen.getByLabelText('When'), { target: { value: 'deleted' } });
    expect(screen.getByText('Save')).toBeDisabled();
    expect(screen.getByText(/Run-prompt steps need/)).toBeInTheDocument();
  });
});
