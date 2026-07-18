import { tauri } from '../tauri';

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
  if (!open) return null;
  return (
    <div
      data-testid="ai-required-scrim"
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
      style={{
        position: 'fixed', inset: 0, background: 'rgba(35,32,27,.45)', zIndex: 1100,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 460, maxWidth: '94%', background: 'var(--cream-pure,#fff)',
          borderRadius: 16, boxShadow: '0 10px 40px rgba(35,32,27,.22)', padding: 20,
        }}
      >
        <div style={{ fontWeight: 700, fontSize: 16, marginBottom: 8 }}>
          {feature} needs an AI provider
        </div>
        <p className="meta" style={{ fontSize: 13, lineHeight: 1.5, marginTop: 0, marginBottom: 16 }}>
          Daisy is local-first. AI features like summaries, analysis, and Q&amp;A run through your own
          API key — add one to use this.
        </p>
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', flexWrap: 'wrap' }}>
          <button className="btn" onClick={onClose}>Close</button>
          <button
            className="btn"
            onClick={() => { onOpenProviders(); onClose(); }}
          >
            Open Providers
          </button>
          <button
            className="btn btn--primary"
            onClick={() => { void tauri.openExternal(AI_HELP_URL); }}
          >
            Get an API key ↗
          </button>
        </div>
      </div>
    </div>
  );
}
