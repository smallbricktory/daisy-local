// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act } from '@testing-library/react';
import { optionLabels, pickOption } from '../test/pick';

// --- Mock Tauri (and heavy deps) before importing the component ---

vi.mock('../tauri', () => ({
  tauri: {
    listSessions: vi.fn().mockResolvedValue([
      {
        session_id: 's1', created_at_unix_seconds: 1, finalized_at_unix_seconds: 1,
        duration_seconds: 60, title: 'Pilot sync', tag_ids: [],
        has_transcript: true, has_dedup: true, has_summary: false,
      },
    ]),
    listPrompts: vi.fn().mockResolvedValue([
      { id: 'builtin:pm', name: 'Project Manager coaching', output: 'sectioned', directive_md: 'Coach for PM voice.', builtin: true },
      { id: 'builtin:daisy', name: 'Daisy Summarizer', output: 'classic', directive_md: '', builtin: true },
      { id: 'u1', name: 'My prompt', output: 'sectioned', directive_md: 'Do my thing.', builtin: false },
    ]),
    savePrompt: vi.fn(),
    runAnalysis: vi.fn().mockResolvedValue({
      prompt_id: 'builtin:pm', prompt_name: 'Project Manager coaching',
      markdown: '**Summary.** Solid meeting.', generated_at_unix_seconds: 10,
    }),
    analysisLoad: vi.fn().mockResolvedValue(null),
  },
  errStr: vi.fn((e: unknown) => String(e)),
}));
const aiState: { configured?: boolean; state?: string; provider?: string | null; hint?: string | null } = { configured: true };
vi.mock('../lib/aiProviderStatus', () => ({
  useAiProviderStatus: () => ({
    configured: aiState.configured ?? true,
    state: aiState.state ?? 'Configured',
    provider: aiState.provider ?? 'groq',
    hint: aiState.hint ?? null,
  }),
}));
vi.mock('../lib/confirm', () => ({ confirm: vi.fn().mockResolvedValue(true) }));
vi.mock('../components/AiProviderRequiredModal', () => ({
  AiProviderRequiredModal: ({ open }: { open: boolean }) => (open ? <div data-testid="ai-modal" /> : null),
}));
vi.mock('../components/MarkdownView', () => ({
  MarkdownView: ({ markdown }: { markdown: string }) => <div data-testid="md">{markdown}</div>,
}));
vi.mock('./SummaryPane', () => ({
  CopyButton: () => <button>Copy</button>,
}));

import { Analyzer } from './Analyzer';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  aiState.configured = true;
  delete aiState.state;
  delete aiState.hint;
});

function topicSelect(): HTMLElement {
  // Two pickers: meeting first, topic second.
  return screen.getAllByRole('combobox')[1];
}

describe('<Analyzer onStartRecording={() => {}} />', () => {
  it('renders prompts plus a permanent Ad Hoc entry in the topic dropdown', async () => {
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const options = await optionLabels(topicSelect());
    expect(options).toContain('Project Manager coaching');
    expect(options).toContain('Daisy Summarizer');
    expect(options).toContain('My prompt');
    expect(options).toContain('Ad Hoc…');
  });

  it('defaults to the last-selected topic from localStorage', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'u1');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    expect(topicSelect().dataset.value).toBe('u1');
  });

  it('selecting a topic exposes its directive for editing', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const ta = screen.getByLabelText(/describe what this analysis should produce/i) as HTMLTextAreaElement;
    expect(ta.value).toBe('Coach for PM voice.');
  });

  it('Run Analysis calls runAnalysis and renders the markdown result', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /run analysis/i }));
    });
    expect(tauri.runAnalysis).toHaveBeenCalledWith({ session_id: 's1', prompt_id: 'builtin:pm' });
    expect(screen.getByTestId('md').textContent).toContain('Solid meeting.');
    expect(screen.getByRole('button', { name: 'Copy' })).toBeInTheDocument();
  });

  it('an edited (dirty) directive runs the text ad hoc instead of the stored id', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'Edited directive.' } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /run analysis/i }));
    });
    expect(tauri.runAnalysis).toHaveBeenCalledWith({ session_id: 's1', directive_md: 'Edited directive.' });
  });

  it('a saved artifact for (session, prompt) is shown on load', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    (tauri.analysisLoad as ReturnType<typeof vi.fn>).mockResolvedValue({
      prompt_id: 'builtin:pm', prompt_name: 'Project Manager coaching',
      markdown: '**Summary.** Prior run.', generated_at_unix_seconds: 5,
    });
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    expect(tauri.analysisLoad).toHaveBeenCalledWith('s1', 'builtin:pm');
    expect(screen.getByTestId('md').textContent).toContain('Prior run.');
  });

  it('Save as… creates a new prompt and selects it', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    (tauri.savePrompt as ReturnType<typeof vi.fn>).mockResolvedValue({
      id: 'new1', name: 'Mine', output: 'sectioned', directive_md: 'Coach for PM voice. tweaked', builtin: false,
    });
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'Coach for PM voice. tweaked' } });
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /save as/i }));
    });
    const nameInput = screen.getByPlaceholderText('New prompt name');
    await act(async () => {
      fireEvent.change(nameInput, { target: { value: 'Mine' } });
      fireEvent.keyDown(nameInput, { key: 'Enter' });
    });
    expect(tauri.savePrompt).toHaveBeenCalledWith({
      id: null, name: 'Mine', directive_md: 'Coach for PM voice. tweaked', output: 'sectioned',
    });
    // Re-fetched the list and selected the created prompt.
    expect(tauri.listPrompts).toHaveBeenCalledTimes(2);
  });

  it('without a configured AI provider, Run Analysis gates on the modal', async () => {
    aiState.configured = false;
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /run analysis/i }));
    });
    expect(tauri.runAnalysis).not.toHaveBeenCalled();
    expect(screen.getByTestId('ai-modal')).toBeInTheDocument();
  });

  it('while installing, Run Analysis shows the download hint instead of the modal', async () => {
    aiState.configured = false;
    aiState.state = 'Installing';
    aiState.hint = "Daisy's model is still downloading.";
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    await act(async () => {
      fireEvent.click(screen.getByRole('button', { name: /run analysis/i }));
    });
    expect(tauri.runAnalysis).not.toHaveBeenCalled();
    expect(screen.queryByTestId('ai-modal')).not.toBeInTheDocument();
    expect(screen.getByText("Daisy's model is still downloading.")).toBeInTheDocument();
  });

  it('a second click on Run Analysis while one is in flight is a no-op', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'builtin:pm');
    const { tauri } = await import('../tauri');
    let resolve!: (v: unknown) => void;
    (tauri.runAnalysis as ReturnType<typeof vi.fn>).mockReturnValue(
      new Promise((res) => { resolve = res; }),
    );
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const runBtn = screen.getByRole('button', { name: /run analysis/i });
    await act(async () => {
      fireEvent.click(runBtn);
      fireEvent.click(runBtn);
    });
    expect(tauri.runAnalysis).toHaveBeenCalledTimes(1);
    await act(async () => {
      resolve({
        prompt_id: 'builtin:pm', prompt_name: 'Project Manager coaching',
        markdown: '**Summary.** Solid meeting.', generated_at_unix_seconds: 10,
      });
    });
  });

  it('switching topics with unsaved edits asks for confirmation', async () => {
    localStorage.setItem('daisy:analyzer:lastPrompt', 'u1');
    const { confirm } = await import('../lib/confirm');
    await act(async () => {
      render(<Analyzer onStartRecording={() => {}} onOpenSession={() => {}} onNavigateToProviders={() => {}} />);
    });
    const ta = screen.getByLabelText(/describe what this analysis should produce/i);
    await act(async () => {
      fireEvent.change(ta, { target: { value: 'changed' } });
    });
    await pickOption(topicSelect(), 'builtin:pm');
    expect(confirm).toHaveBeenCalled();
  });
});
