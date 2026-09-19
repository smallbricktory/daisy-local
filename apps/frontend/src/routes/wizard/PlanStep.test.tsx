// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { readFileSync } from 'node:fs';
import { PlanStep } from './PlanStep';
import type { DownloadJob, OnboardingPlan } from '../../tauri';

const { jobs, finishedRef } = vi.hoisted(() => ({
  jobs: {} as Record<string, DownloadJob | undefined>,
  finishedRef: { current: 0 },
}));
const onboardingApply = vi.fn<() => Promise<OnboardingPlan>>();
const onboardingPlan = vi.fn<() => Promise<OnboardingPlan>>();

vi.mock('../../tauri', () => ({
  tauri: { onboardingApply: () => onboardingApply(), onboardingPlan: () => onboardingPlan() },
  errStr: (e: unknown) => String(e),
}));
vi.mock('../../downloadQueue', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../downloadQueue')>()),
  useDownloadJob: (id: string) => jobs[id],
  useDownloadsFinished: () => finishedRef.current,
}));

const LLM_ID = 'gemma-4-e4b-it-q4';
const PARAKEET_ID = 'parakeet-tdt-0.6b-v2-int8';

const PLAN = (over: Partial<OnboardingPlan> = {}): OnboardingPlan => ({
  ai: { kind: 'builtin', capability: 'fast', installed: true },
  transcription: { engine: 'parakeet', installed: true },
  profile_dir: '/Users/alex/Library/Application Support/ai.daisy.Daisy',
  ...over,
});

const job = (phase: DownloadJob['phase'], id: string, kind: DownloadJob['kind']): DownloadJob => ({
  kind, id, indeterminate: false, phase,
});

describe('PlanStep', () => {
  beforeEach(() => {
    for (const k of Object.keys(jobs)) delete jobs[k];
    finishedRef.current = 0;
    onboardingApply.mockReset().mockResolvedValue(PLAN());
    onboardingPlan.mockReset().mockResolvedValue(PLAN());
  });

  it('applies the plan on mount and names the fast built-in model', async () => {
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByText("Here's how Daisy is set up")).toBeInTheDocument();
    expect(onboardingApply).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId('plan-row-ai')).toHaveTextContent(
      "Daisy's built-in model · fast on this computer",
    );
    expect(screen.getByTestId('plan-status-ai')).toHaveTextContent('ready');
  });

  it('a machine with no AI names the hardware and where cloud providers live', async () => {
    onboardingApply.mockResolvedValue(PLAN({
      ai: { kind: 'none', reason: 'needs a graphics card with 6 GB of memory or more' },
    }));
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-row-ai')).toHaveTextContent(
      'Off — needs a graphics card with 6 GB of memory or more. '
      + 'Cloud providers are available under Settings → Providers, with Nerd Mode on.',
    );
    expect(screen.queryByTestId('plan-status-ai')).not.toBeInTheDocument();
  });

  it('an installing AI model reads queued, not ready', async () => {
    onboardingApply.mockResolvedValue(PLAN({ ai: { kind: 'builtin', capability: 'fast', installed: false } }));
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-ai')).toHaveTextContent('queued');
  });

  it('macOS names Apple speech recognition and reports readiness', async () => {
    onboardingApply.mockResolvedValue(PLAN({
      transcription: { engine: 'speech_analyzer', ready: true },
    }));
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-row-transcription')).toHaveTextContent(
      'Apple speech recognition',
    );
    expect(screen.getByTestId('plan-status-transcription')).toHaveTextContent('ready');
  });

  it('shows each phase of the download for the engine row', async () => {
    onboardingApply.mockResolvedValue(PLAN({
      transcription: { engine: 'parakeet', installed: false },
    }));
    jobs[PARAKEET_ID] = job({ state: 'downloading', pct: 0.12 }, PARAKEET_ID, 'asr');
    const first = render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('downloading 12%');
    first.unmount();

    jobs[PARAKEET_ID] = job({ state: 'warming' }, PARAKEET_ID, 'asr');
    const second = render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('preparing…');
    second.unmount();

    jobs[PARAKEET_ID] = job({ state: 'retrying', attempt: 2, of: 5, error: 'timed out' }, PARAKEET_ID, 'asr');
    const retrying = render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('retrying, attempt 2 of 5');
    retrying.unmount();

    jobs[PARAKEET_ID] = job({ state: 'failed', error: 'disk full' }, PARAKEET_ID, 'asr');
    const third = render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('failed: There is not enough free space for this model');
    third.unmount();

    jobs[PARAKEET_ID] = job({ state: 'queued' }, PARAKEET_ID, 'asr');
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('queued');
  });

  it('reports the AI download under the AI row', async () => {
    jobs[LLM_ID] = job({ state: 'downloading', pct: 0.42 }, LLM_ID, 'llm');
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-ai')).toHaveTextContent('downloading 42%');
  });

  it('points to Nerd Mode for a cloud provider', async () => {
    render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByText(
      'To use a cloud provider, turn on Nerd Mode in Settings → Display, then open Providers.',
    )).toBeInTheDocument();
  });

  it('names where recordings are saved and finishes whatever the plan says', async () => {
    const onFinish = vi.fn();
    render(<PlanStep onFinish={onFinish} />);
    expect(await screen.findByTestId('plan-row-recordings')).toHaveTextContent(
      '/Users/alex/Library/Application Support/ai.daisy.Daisy',
    );
    const finish = screen.getByTestId('wizard-finish');
    expect(finish).toBeEnabled();
    fireEvent.click(finish);
    expect(onFinish).toHaveBeenCalledTimes(1);
  });

  it('a job leaving the queue after finishing flips the row to ready', async () => {
    onboardingApply.mockResolvedValue(PLAN({ transcription: { engine: 'parakeet', installed: false } }));
    jobs[PARAKEET_ID] = job({ state: 'downloading', pct: 0.9 }, PARAKEET_ID, 'asr');
    const { rerender } = render(<PlanStep onFinish={() => {}} />);
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('downloading 90%');

    // The job finishes and drops out of the queue; a fresh plan says installed.
    delete jobs[PARAKEET_ID];
    onboardingPlan.mockResolvedValue(PLAN({ transcription: { engine: 'parakeet', installed: true } }));
    finishedRef.current = 1;
    rerender(<PlanStep onFinish={() => {}} />);

    await waitFor(() => expect(onboardingPlan).toHaveBeenCalledTimes(1));
    expect(await screen.findByTestId('plan-status-transcription')).toHaveTextContent('ready');
  });

  it('a failed apply still renders the rows and offers Try again', async () => {
    onboardingApply.mockRejectedValueOnce(new Error('apply failed'));
    const onFinish = vi.fn();
    render(<PlanStep onFinish={onFinish} />);
    await waitFor(() => expect(screen.getByText(/apply failed/)).toBeInTheDocument());
    expect(screen.getByTestId('plan-row-ai')).toBeInTheDocument();
    expect(screen.getByTestId('plan-row-transcription')).toBeInTheDocument();
    expect(screen.getByTestId('plan-row-recordings')).toBeInTheDocument();

    const finish = screen.getByTestId('wizard-finish');
    expect(finish).toBeEnabled();
    fireEvent.click(finish);
    expect(onFinish).toHaveBeenCalledTimes(1);

    onboardingApply.mockResolvedValueOnce(PLAN());
    fireEvent.click(screen.getByTestId('plan-retry'));
    await waitFor(() => expect(onboardingApply).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByText(/apply failed/)).not.toBeInTheDocument());
  });

  it('a failed apply stays busy until the fallback plan has landed', async () => {
    onboardingApply.mockRejectedValueOnce(new Error('apply failed'));
    let resolvePlan: (plan: OnboardingPlan) => void = () => {};
    onboardingPlan.mockImplementationOnce(() => new Promise((resolve) => { resolvePlan = resolve; }));
    render(<PlanStep onFinish={() => {}} />);

    await waitFor(() => expect(screen.getByText(/apply failed/)).toBeInTheDocument());
    expect(screen.queryByTestId('plan-row-ai')).not.toBeInTheDocument();
    expect(screen.getByTestId('wizard-finish')).toHaveTextContent('Setting up…');

    resolvePlan(PLAN());
    await waitFor(() =>
      expect(screen.getByTestId('wizard-finish')).toHaveTextContent('Start using Daisy'));
    expect(screen.getByTestId('plan-row-ai')).toBeInTheDocument();
  });

  it('shows a busy label on Finish while the plan is applying, without disabling it', async () => {
    let resolveApply: (plan: OnboardingPlan) => void = () => {};
    onboardingApply.mockImplementation(() => new Promise((resolve) => { resolveApply = resolve; }));
    const onFinish = vi.fn();
    render(<PlanStep onFinish={onFinish} />);

    const finish = screen.getByTestId('wizard-finish');
    expect(finish).toHaveTextContent('Setting up…');
    expect(finish).toBeEnabled();
    fireEvent.click(finish);
    expect(onFinish).toHaveBeenCalledTimes(1);

    resolveApply(PLAN());
    await waitFor(() => expect(finish).toHaveTextContent('Start using Daisy'));
  });

  it('uses the registry ids the queue tags its jobs with', () => {
    const registry = readFileSync('../../crates/providers-local/src/registry.rs', 'utf8');
    for (const id of [LLM_ID, PARAKEET_ID, 'speechanalyzer']) {
      expect(registry).toContain(`id: "${id}"`);
    }
  });
});
