import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';

vi.mock('../tauri', () => ({
  tauri: {
    listPrompts: vi.fn().mockResolvedValue([
      { id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: '', builtin: true },
      { id: 'builtin:zoom', name: 'Zoom-style Meeting Notes', output: 'sectioned', directive_md: 'Narrative sections.', builtin: true },
      { id: 'u1', name: 'Mine', output: 'sectioned', directive_md: 'my text', builtin: false },
    ]),
    savePrompt: vi.fn(),
    deletePrompt: vi.fn().mockResolvedValue(undefined),
    resetPrompt: vi.fn().mockResolvedValue(undefined),
    setDefaultSummaryPrompt: vi.fn().mockResolvedValue(undefined),
  },
  errStr: vi.fn((e: unknown) => String(e)),
}));
vi.mock('../lib/confirm', () => ({ confirm: vi.fn().mockResolvedValue(true) }));
vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <div data-testid="example">{markdown}</div>,
}));
// PromptsSection lives inside Settings.tsx; the heavy sibling imports are
// mocked and the module loads in jsdom.
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn() }));
vi.mock('../components/MicLevel', () => ({ MicLevel: () => null }));
vi.mock('../components/McpSettings', () => ({ McpSettings: () => null }));
vi.mock('../components/ProviderEditor', () => ({ ProviderEditor: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../components/FieldError', () => ({
  FieldError: ({ children }: { children?: React.ReactNode }) => (children ? <p>{children}</p> : null),
  FieldOk: () => null,
}));
vi.mock('../recordingsJob', () => ({
  subscribeRecJob: vi.fn(() => () => {}),
  getRecJobState: vi.fn(() => ({ phase: 'idle' })),
  startDeleteAll: vi.fn(),
}));
vi.mock('../lib/aiProviderStatus', () => ({
  revalidateAiProviderStatus: vi.fn(),
  useAiProviderStatus: () => ({ configured: true }),
}));
vi.mock('../lib/toastStore', () => ({ pushToast: vi.fn(), updateToast: vi.fn() }));
vi.mock('../lib/gatewayNotice', () => ({ showGatewayNoticeIfNeeded: vi.fn(() => false) }));

import { PromptsSection } from './Settings';
import type { Settings as SettingsT } from '../tauri';

const SETTINGS = { default_summary_prompt_id: 'builtin:daisy' } as unknown as SettingsT;

beforeEach(() => {
  vi.clearAllMocks();
});

async function renderSection(update = vi.fn()) {
  await act(async () => {
    render(<PromptsSection settings={SETTINGS} update={update} />);
  });
  return update;
}

function dropdown(): HTMLSelectElement {
  return screen.getByRole('combobox') as HTMLSelectElement;
}

describe('<PromptsSection />', () => {
  it('lists all prompts with a default marker and shows the selected directive', async () => {
    await renderSection();
    const opts = Array.from(dropdown().options).map((o) => o.textContent);
    expect(opts).toEqual(['Daisy Summarizer · default', 'Zoom-style Meeting Notes', 'Mine']);
    // First prompt selected; its (empty) directive shown under the fixed label.
    expect(screen.getByLabelText(/describe what this analysis should produce/i)).toBeInTheDocument();
  });

  it('built-in: Save hidden, example output rendered, Save-as creates a copy', async () => {
    const { tauri } = await import('../tauri');
    (tauri.savePrompt as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: 'new1', name: 'My Zoom', output: 'sectioned', directive_md: 'Narrative sections. more', builtin: false,
    });
    await renderSection();
    await act(async () => {
      fireEvent.change(dropdown(), { target: { value: 'builtin:zoom' } });
    });
    expect(screen.queryByRole('button', { name: /^save$/i })).not.toBeInTheDocument();
    expect(screen.getByTestId('example')).toBeInTheDocument(); // built-ins show an example
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'Narrative sections. more' } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /save as/i }));
    });
    const nameInput = screen.getByPlaceholderText('New prompt name');
    await act(async () => {
      fireEvent.change(nameInput, { target: { value: 'My Zoom' } });
      fireEvent.keyDown(nameInput, { key: 'Enter' });
    });
    expect(tauri.savePrompt).toHaveBeenCalledWith({
      id: null, name: 'My Zoom', directive_md: 'Narrative sections. more', output: 'sectioned',
    });
  });

  it('user prompt: Save enabled when dirty and saves in place', async () => {
    const { tauri } = await import('../tauri');
    (tauri.savePrompt as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: 'u1', name: 'Mine', output: 'sectioned', directive_md: 'my text v2', builtin: false,
    });
    await renderSection();
    await act(async () => {
      fireEvent.change(dropdown(), { target: { value: 'u1' } });
    });
    const save = screen.getByRole('button', { name: /^save$/i });
    expect(save).toBeDisabled();
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'my text v2' } });
    });
    expect(save).toBeEnabled();
    await act(async () => {
      fireEvent.click(save);
    });
    expect(tauri.savePrompt).toHaveBeenCalledWith({
      id: 'u1', name: 'Mine', directive_md: 'my text v2', output: 'sectioned',
    });
  });

  it('Default-for-summaries wires through; Delete confirms and deletes', async () => {
    const { tauri } = await import('../tauri');
    const { confirm } = await import('../lib/confirm');
    const update = await renderSection();
    // Make the user prompt the default first.
    await act(async () => {
      fireEvent.change(dropdown(), { target: { value: 'u1' } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /default for summaries/i }));
    });
    expect(tauri.setDefaultSummaryPrompt).toHaveBeenCalledWith('u1');
    expect(update).toHaveBeenCalledWith('default_summary_prompt_id', 'u1');
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /delete/i }));
    });
    expect(confirm).toHaveBeenCalled();
    expect(tauri.deletePrompt).toHaveBeenCalledWith('u1');
  });

  it('Summarizer: short edit shows inline error, valid edit saves, Reset restores', async () => {
    const { tauri } = await import('../tauri');
    const { confirm } = await import('../lib/confirm');
    (tauri.savePrompt as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: 'x'.repeat(60), builtin: true,
    });
    await renderSection();
    // Daisy selected by default: name locked, directive editable, Reset offered.
    expect((screen.getByLabelText(/^name/i) as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByRole('button', { name: /reset to default/i })).toBeInTheDocument();
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    // Too short → inline error, no backend call.
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'too short' } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^save$/i }));
    });
    expect(screen.getByText(/at least 40 characters/i)).toBeInTheDocument();
    expect(tauri.savePrompt).not.toHaveBeenCalled();
    // Long enough → saves in place with the shipped name.
    const long = 'Summarize in three bullets, then list every date mentioned in the meeting.';
    await act(async () => {
      fireEvent.change(ta, { target: { value: long } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /^save$/i }));
    });
    expect(tauri.savePrompt).toHaveBeenCalledWith({
      id: 'builtin:daisy', name: 'Daisy Summarizer', directive_md: long, output: 'classic',
    });
    // Reset → confirm + backend reset.
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /reset to default/i }));
    });
    expect(confirm).toHaveBeenCalled();
    expect(tauri.resetPrompt).toHaveBeenCalledWith('builtin:daisy');
  });

  it('switching prompts with unsaved edits asks for confirmation', async () => {
    const { confirm } = await import('../lib/confirm');
    await renderSection();
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'edited' } });
    });
    await act(async () => {
      fireEvent.change(dropdown(), { target: { value: 'u1' } });
    });
    expect(confirm).toHaveBeenCalled();
  });
});
