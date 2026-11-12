// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useState } from 'react';
import { tauri, type ActionStep, type IntegrationPublic, type Prompt, type WorkflowTrigger } from '../../tauri';
import { Select } from '../Select';

function RunPromptEditor({ step, onChange, prompts, integrations, index }: {
  step: Extract<ActionStep, { type: 'run_prompt' }>;
  onChange: (s: ActionStep) => void;
  prompts: Prompt[];
  integrations: IntegrationPublic[];
  /** This step's position in the list, for the test ids. */
  index: number;
}) {
  return (
    <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">Run prompt</span>
        <Select
          testId={`step-prompt-${index}`}
          label="Prompt"
          value={step.prompt_id}
          onChange={(v) => onChange({ ...step, prompt_id: v })}
          options={[
            { value: '', label: 'Choose a prompt…' },
            ...prompts.map((p) => ({ value: p.id, label: p.name })),
          ]}
        />
      </span>
      <span className="meta">Output is stored with the session (Analyzer).</span>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">also send to</span>
        {integrations.length === 0 ? (
          <span className="meta">No destinations — add one in Settings → Integrations.</span>
        ) : (
          <Select
            testId={`step-send-to-${index}`}
            label="Also send to integration"
            value={step.send_to_integration ?? ''}
            onChange={(v) => onChange({ ...step, send_to_integration: v || null })}
            options={[
              { value: '', label: '—' },
              ...integrations.map((i) => ({ value: i.id, label: i.name })),
            ]}
          />
        )}
      </span>
      <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
        <span className="meta">also write to folder</span>
        {step.write_to_dir ? (
          <>
            <span className="mono" style={{ fontSize: 'var(--t-small)' }}>{step.write_to_dir}</span>
            <button data-testid={`step-clear-dir-${index}`} type="button" className="btn" title="Clear folder" onClick={() => onChange({ ...step, write_to_dir: null })}>✕</button>
          </>
        ) : (
          <button
            data-testid={`step-choose-dir-${index}`}
            type="button"
            className="btn"
            onClick={() => {
              void tauri.pickDirectory('workflow-output').then((dir) => {
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
          <span className="mono" style={{ fontSize: 'var(--t-small)', paddingTop: 4 }}>{i + 1}.</span>
          <div style={{ flex: 1 }}>
            {step.type === 'run_prompt' ? (
              <RunPromptEditor step={step} onChange={(s) => setStep(i, s)} prompts={prompts} integrations={integrations} index={i} />
            ) : (
              <span style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}>
                <span className="meta">Send to</span>
                {integrations.length === 0 ? (
                  <span className="meta">No destinations — add one in Settings → Integrations.</span>
                ) : (
                  <Select
                    testId={`step-integration-${i}`}
                    label="Integration"
                    value={step.integration_id}
                    onChange={(v) => setStep(i, { type: 'push_integration', integration_id: v })}
                    options={[
                      { value: '', label: 'Choose a destination…' },
                      ...integrations.map((x) => ({ value: x.id, label: x.name })),
                    ]}
                  />
                )}
                {trigger !== 'finalized' && (
                  <span className="meta">Sends event details only (recording content isn't available for this trigger).</span>
                )}
              </span>
            )}
          </div>
          <button data-testid={`step-up-${i}`} type="button" className="btn" title="Move up" disabled={i === 0} onClick={() => move(i, -1)}>↑</button>
          <button data-testid={`step-down-${i}`} type="button" className="btn" title="Move down" disabled={i === steps.length - 1} onClick={() => move(i, 1)}>↓</button>
          <button data-testid={`step-remove-${i}`} type="button" className="btn" title="Remove step" onClick={() => onChange(steps.filter((_, x) => x !== i))}>✕</button>
        </div>
      ))}
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <button data-testid="step-add" type="button" className="btn" onClick={() => setMenuOpen((o) => !o)}>+ step</button>
        {menuOpen && (
          <>
            {trigger === 'finalized' && (
              <button
                data-testid="step-add-prompt"
                type="button"
                className="btn"
                onClick={() => add({ type: 'run_prompt', prompt_id: '', send_to_integration: null, write_to_dir: null })}
              >
                Run prompt
              </button>
            )}
            <button
              data-testid="step-add-integration"
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
