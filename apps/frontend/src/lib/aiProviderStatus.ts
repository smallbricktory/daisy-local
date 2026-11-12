// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Shared "is an AI provider usable?" status for the LLM-gated features
// (summary, Q&A, chapters, analysis, transcript polish). Read from the
// built-in model store, which follows the backend's `builtin-model:changed`.

import { useMemo } from 'react';
import { useBuiltinModel } from '../builtinModel';
import type { SummaryProviderStatusKind } from '../tauri';

export interface AiProviderStatus {
  /** True only when a provider is selected AND its creds are usable. */
  configured: boolean;
  /** 'None' = user picked no provider. Otherwise the backend status. */
  state: SummaryProviderStatusKind | 'loading';
  provider: string | null;
  hint: string | null;
}

const LOADING: AiProviderStatus = {
  configured: false,
  state: 'loading',
  provider: null,
  hint: null,
};

/** The AI-provider status, `loading` until the backend first answers. */
export function useAiProviderStatus(): AiProviderStatus {
  const provider = useBuiltinModel()?.provider;
  return useMemo(
    () => provider
      ? { configured: provider.state === 'Configured', state: provider.state, provider: provider.provider, hint: provider.hint }
      : LOADING,
    [provider],
  );
}
