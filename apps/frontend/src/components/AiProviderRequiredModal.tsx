// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { tauri, errStr, formatBytes } from '../tauri';
import { enqueueDownload } from '../downloadQueue';
import { useBuiltinModel } from '../builtinModel';
import { useNerdMode } from '../lib/nerdMode';

export const AI_HELP_URL = 'https://www.daisylocal.app/help/getting-an-api-key';

/** Shown when a user invokes an LLM-gated feature (Analysis, Q&A) with no usable
 *  AI provider. Purely presentational — the caller owns `open` state and the
 *  not-configured check. Scrim styling mirrors SpeakerLabelModalView. */
export function AiProviderRequiredModal({
  open,
  feature,
  onClose,
  onOpenProviders,
}: {
  open: boolean;
  feature: string;
  onClose: () => void;
  onOpenProviders: () => void;
}) {
  const status = useBuiltinModel();
  const builtin = status && status.model.capability !== 'unsupported' ? status.model : null;
  const [err, setErr] = useState<string | null>(null);
  const nerd = useNerdMode();
  useEffect(() => { if (open) setErr(null); }, [open]);

  // An installed built-in model becomes the provider in place; an
  // uninstalled one is downloaded from Providers.
  const selectInstalled = async () => {
    setErr(null);
    try {
      await tauri.setAiProvider('local');
      onClose();
    } catch (e) {
      setErr(errStr(e));
    }
  };

  if (!open) return null;
  // "Get an API key" carries the primary emphasis only when the built-in
  // model isn't offered.
  const getKeyClass = builtin ? 'btn' : 'btn btn--primary';
  return (
    <div
      data-testid="ai-required-scrim"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      className="modal-backdrop"
    >
      <div
        data-testid="ai-required-modal"
        onClick={(e) => e.stopPropagation()}
        className="modal"
        style={{ width: 460 }}
      >
        <div className="modal__title">
          {feature} needs an AI provider
        </div>
        <p className="meta" style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 0, marginBottom: 16 }}>
          Daisy is local-first. AI features like summaries, analysis, and Q&amp;A run on Daisy's
          built-in model on this computer, or through your own API key.
        </p>
        {!nerd && (
          <p
            data-testid="ai-required-nerd-hint"
            className="meta"
            style={{ fontSize: 'var(--t-ui)', lineHeight: 'var(--lh-text)', marginTop: 0, marginBottom: 16 }}
          >
            To use a cloud provider, turn on Nerd Mode in Settings → Display.
          </p>
        )}
        {err && <p data-testid="ai-required-error" className="meta meta--danger">{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', flexWrap: 'wrap' }}>
          {builtin && (
            <button
              data-testid="ai-required-builtin"
              className="btn btn--primary"
              onClick={builtin.installed
                ? () => { void selectInstalled(); }
                : () => {
                    enqueueDownload('llm', builtin.id).catch((e) => setErr(errStr(e)));
                    onOpenProviders();
                    onClose();
                  }}
            >
              {builtin.installed
                ? "Use Daisy's built-in model"
                : `Use Daisy's built-in model (${formatBytes(builtin.bytes)} download)`}
            </button>
          )}
          <button data-testid="ai-required-close" className="btn" onClick={onClose}>Close</button>
          <button
            data-testid="ai-required-open-providers"
            className="btn"
            onClick={() => { onOpenProviders(); onClose(); }}
          >
            Open Providers
          </button>
          <button
            data-testid="ai-required-get-key"
            className={getKeyClass}
            onClick={() => { void tauri.openExternal(AI_HELP_URL); }}
          >
            Get an API key ↗
          </button>
        </div>
      </div>
    </div>
  );
}
