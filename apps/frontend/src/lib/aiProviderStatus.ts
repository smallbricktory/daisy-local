// Shared "is an AI provider usable?" status for the LLM-gated features
// (summary, Q&A, chapters, analysis, transcript polish). Backed by the
// `summary_provider_status` Tauri command. A small module-level cache plus a
// revalidate-on-demand keeps every banner consistent without each route
// re-querying on its own cadence.

import { useEffect, useState } from 'react';
import { summaryProviderStatus, type SummaryProviderStatusKind } from '../tauri';

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

let cache: AiProviderStatus | null = null;
const listeners = new Set<(s: AiProviderStatus) => void>();

async function fetchStatus(): Promise<void> {
  try {
    const r = await summaryProviderStatus();
    cache = {
      configured: r.state === 'Configured',
      state: r.state,
      provider: r.provider,
      hint: r.hint,
    };
  } catch {
    // Vault locked / command error — treat as not configured but don't crash.
    cache = { configured: false, state: 'VaultLocked', provider: null, hint: null };
  }
  for (const l of listeners) l(cache);
}

/** Force a refresh (call after saving Settings or unlocking the vault). */
export function revalidateAiProviderStatus(): void {
  void fetchStatus();
}

/** Subscribe to AI-provider status. Fetches once on first mount and shares the
 *  result across all subscribers. */
export function useAiProviderStatus(): AiProviderStatus {
  const [status, setStatus] = useState<AiProviderStatus>(cache ?? LOADING);

  useEffect(() => {
    listeners.add(setStatus);
    if (cache == null) {
      void fetchStatus();
    } else {
      setStatus(cache);
    }
    return () => {
      listeners.delete(setStatus);
    };
  }, []);

  return status;
}
