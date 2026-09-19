// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { AiProviderRequiredModal, AI_HELP_URL } from './AiProviderRequiredModal';
import { enqueueDownload } from '../downloadQueue';
import type { DownloadJob, LlmModelInfo, ProviderId } from '../tauri';

const openExternal = vi.fn();
const listLlmModels = vi.fn<() => Promise<LlmModelInfo[]>>(() => Promise.resolve([]));
const setAiProvider = vi.fn<(p: ProviderId | null) => Promise<void>>(() => Promise.resolve());
const revalidate = vi.fn();
vi.mock('../tauri', () => ({
  tauri: {
    openExternal: (url: string) => openExternal(url),
    listLlmModels: () => listLlmModels(),
    setAiProvider: (p: ProviderId | null) => setAiProvider(p),
  },
  errStr: (e: unknown) => String(e),
  formatBytes: (n: number) => `${(n / 1_000_000_000).toFixed(1)} GB`,
}));
vi.mock('../lib/aiProviderStatus', () => ({ revalidateAiProviderStatus: () => revalidate() }));
vi.mock('../lib/nerdMode', () => ({ useNerdMode: () => true }));

let mockJob: DownloadJob | undefined;
vi.mock('../downloadQueue', () => ({
  enqueueDownload: vi.fn().mockResolvedValue(undefined),
  useDownloadJob: (id: string) => (mockJob && mockJob.id === id ? mockJob : undefined),
}));

const UNINSTALLED: LlmModelInfo = {
  id: 'gemma-4-e4b-it-q4', display: 'Gemma 4 E4B', installed: false,
  bytes: 5_130_000_000, license: 'Gemma Terms of Use', license_url: 'https://ai.google.dev/gemma/terms', capability: 'fast', capability_line: 'Fast on this machine.', partial_bytes: 0,
};

const downloadingJob = (id: string): DownloadJob => ({
  kind: 'llm', id, indeterminate: false,
  phase: { state: 'downloading', pct: 0.5 },
});

describe('AiProviderRequiredModal', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    openExternal.mockClear();
    listLlmModels.mockClear();
    listLlmModels.mockResolvedValue([]);
    setAiProvider.mockClear();
    setAiProvider.mockResolvedValue();
    revalidate.mockClear();
    mockJob = undefined;
  });

  it('renders nothing when closed', () => {
    const { container } = render(
      <AiProviderRequiredModal open={false} feature="Analysis" onClose={() => {}} onOpenProviders={() => {}} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the feature name', () => {
    render(<AiProviderRequiredModal open feature="Analysis" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(screen.getByText(/Analysis needs an AI provider/i)).toBeInTheDocument();
  });

  it('Get an API key opens the external help page', () => {
    render(<AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /get an api key/i }));
    expect(openExternal).toHaveBeenCalledWith(AI_HELP_URL);
  });

  it('Open Providers navigates and closes', () => {
    const onClose = vi.fn();
    const onOpenProviders = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={onOpenProviders} />);
    fireEvent.click(screen.getByRole('button', { name: /open providers/i }));
    expect(onOpenProviders).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Close calls onClose', () => {
    const onClose = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /close/i }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('offers the built-in model when one is uninstalled and supported', async () => {
    listLlmModels.mockResolvedValue([UNINSTALLED]);
    render(<AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
  });

  it('picking the built-in model starts its download and opens Providers', async () => {
    listLlmModels.mockResolvedValue([UNINSTALLED]);
    const onClose = vi.fn();
    const onOpenProviders = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={onOpenProviders} />);
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-required-builtin'));
    expect(onOpenProviders).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(enqueueDownload).toHaveBeenCalledWith('llm', UNINSTALLED.id);
  });

  it('revalidates once when the modal\'s own download leaves the queue', async () => {
    listLlmModels.mockResolvedValue([UNINSTALLED]);
    const { rerender } = render(
      <AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />,
    );
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-required-builtin'));

    mockJob = downloadingJob(UNINSTALLED.id);
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(revalidate).not.toHaveBeenCalled();

    mockJob = undefined;
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(revalidate).toHaveBeenCalledTimes(1);

    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(revalidate).toHaveBeenCalledTimes(1);
  });

  it('does not revalidate when the modal\'s own download fails', async () => {
    listLlmModels.mockResolvedValue([UNINSTALLED]);
    const { rerender } = render(
      <AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />,
    );
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-required-builtin'));

    mockJob = downloadingJob(UNINSTALLED.id);
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);

    mockJob = { ...downloadingJob(UNINSTALLED.id), phase: { state: 'failed', error: 'no route to host' } };
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);

    mockJob = undefined;
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(revalidate).not.toHaveBeenCalled();
  });

  it('does not revalidate when a different job disappears from the queue', async () => {
    listLlmModels.mockResolvedValue([UNINSTALLED]);
    const { rerender } = render(
      <AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />,
    );
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-required-builtin'));

    mockJob = downloadingJob('some-other-model');
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);

    mockJob = undefined;
    rerender(<AiProviderRequiredModal open={false} feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(revalidate).not.toHaveBeenCalled();
  });

  it('an installed built-in model becomes the provider without leaving the page', async () => {
    listLlmModels.mockResolvedValue([{ ...UNINSTALLED, installed: true }]);
    const onClose = vi.fn();
    const onOpenProviders = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={onOpenProviders} />);
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toHaveTextContent("Use Daisy's built-in model"));
    fireEvent.click(screen.getByTestId('ai-required-builtin'));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(setAiProvider).toHaveBeenCalledWith('local');
    expect(revalidate).toHaveBeenCalledTimes(1);
    expect(onOpenProviders).not.toHaveBeenCalled();
    expect(enqueueDownload).not.toHaveBeenCalled();
  });

  it('a failed save keeps the modal open with the error', async () => {
    listLlmModels.mockResolvedValue([{ ...UNINSTALLED, installed: true }]);
    setAiProvider.mockRejectedValue('disk full');
    const onClose = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={() => {}} />);
    await waitFor(() => expect(screen.getByTestId('ai-required-builtin')).toBeInTheDocument());
    fireEvent.click(screen.getByTestId('ai-required-builtin'));
    await waitFor(() => expect(screen.getByTestId('ai-required-error')).toHaveTextContent('disk full'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('offers no built-in model when every row is unsupported', async () => {
    listLlmModels.mockResolvedValue([{ ...UNINSTALLED, capability: 'unsupported' }]);
    render(<AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    await waitFor(() => expect(listLlmModels).toHaveBeenCalled());
    expect(screen.queryByTestId('ai-required-builtin')).not.toBeInTheDocument();
  });
});
