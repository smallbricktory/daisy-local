import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import type { ActionStep, IntegrationPublic, Prompt, WorkflowTrigger } from '../../tauri';

const mono: React.CSSProperties = { fontFamily: 'var(--font-mono)', fontSize: 12 };

function RunPromptEditor({ step, onChange, prompts, integrations }: {
  step: Extract<ActionStep, { type: 'run_prompt' }>;
  onChange: (s: ActionStep) => void;
  prompts: Prompt[];
  integrations: IntegrationPublic[];
}) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">Run prompt</span>
        <select
          aria-label="Prompt"
          value={step.prompt_id}
          onChange={(e) => onChange({ ...step, prompt_id: e.target.value })}
        >
          <option value="">Choose a prompt…</option>
          {prompts.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
        </select>
      </span>
      <span className="meta">Output is stored with the session (Analyzer).</span>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">also send to</span>
        {integrations.length === 0 ? (
          <span className="meta">No destinations — add one in Settings → Integrations.</span>
        ) : (
          <select
            aria-label="Also send to integration"
            value={step.send_to_integration ?? ''}
            onChange={(e) => onChange({ ...step, send_to_integration: e.target.value || null })}
          >
            <option value="">—</option>
            {integrations.map((i) => <option key={i.id} value={i.id}>{i.name}</option>)}
          </select>
        )}
      </span>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">also write to folder</span>
        {step.write_to_dir ? (
          <>
            <span style={mono}>{step.write_to_dir}</span>
            <button type="button" className="btn" title="Clear folder" onClick={() => onChange({ ...step, write_to_dir: null })}>✕</button>
          </>
        ) : (
          <button
            type="button"
            className="btn"
            onClick={() => {
              void open({ directory: true }).then((dir) => {
                if (typeof dir === 'string' && dir) onChange({ ...step, write_to_dir: dir });
              });
            }}
          >
            Choose folder…
          </button>
        )}
      </span>
    </div>
  );
}

export function ActionStepsEditor({ steps, onChange, trigger, prompts, integrations }: {
  steps: ActionStep[];
  onChange: (s: ActionStep[]) => void;
  trigger: WorkflowTrigger;
  prompts: Prompt[];
  integrations: IntegrationPublic[];
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const setStep = (i: number, s: ActionStep) => {
    const next = steps.slice();
    next[i] = s;
    onChange(next);
  };
  const move = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= steps.length) return;
    const next = steps.slice();
    [next[i], next[j]] = [next[j], next[i]];
    onChange(next);
  };
  const add = (s: ActionStep) => {
    onChange([...steps, s]);
    setMenuOpen(false);
  };
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 10 }}>
      {steps.map((step, i) => (
        <div key={i} style={{ display: 'flex', alignItems: 'flex-start', gap: 8 }}>
          <span style={{ ...mono, paddingTop: 4 }}>{i + 1}.</span>
          <div style={{ flex: 1 }}>
            {step.type === 'run_prompt' ? (
              <RunPromptEditor step={step} onChange={(s) => setStep(i, s)} prompts={prompts} integrations={integrations} />
            ) : (
              <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
                <span className="meta">Send to</span>
                {integrations.length === 0 ? (
                  <span className="meta">No destinations — add one in Settings → Integrations.</span>
                ) : (
                  <select
                    aria-label="Integration"
                    value={step.integration_id}
                    onChange={(e) => setStep(i, { type: 'push_integration', integration_id: e.target.value })}
                  >
                    <option value="">Choose a destination…</option>
                    {integrations.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
                  </select>
                )}
                {trigger !== 'finalized' && (
                  <span className="meta">Sends event details only (recording content isn't available for this trigger).</span>
                )}
              </span>
            )}
          </div>
          <button type="button" className="btn" title="Move up" disabled={i === 0} onClick={() => move(i, -1)}>↑</button>
          <button type="button" className="btn" title="Move down" disabled={i === steps.length - 1} onClick={() => move(i, 1)}>↓</button>
          <button type="button" className="btn" title="Remove step" onClick={() => onChange(steps.filter((_, x) => x !== i))}>✕</button>
        </div>
      ))}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <button type="button" className="btn" onClick={() => setMenuOpen((o) => !o)}>+ step</button>
        {menuOpen && (
          <>
            {trigger === 'finalized' && (
              <button
                type="button"
                className="btn"
                onClick={() => add({ type: 'run_prompt', prompt_id: '', send_to_integration: null, write_to_dir: null })}
              >
                Run prompt
              </button>
            )}
            <button
              type="button"
              className="btn"
              onClick={() => add({ type: 'push_integration', integration_id: integrations[0]?.id ?? '' })}
            >
              Send to integration
            </button>
          </>
        )}
      </div>
    </div>
  );
}
