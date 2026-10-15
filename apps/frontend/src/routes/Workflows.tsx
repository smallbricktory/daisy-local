// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useEffect, useState } from 'react';
import { tauri, type ActionStep, type ConditionNode, type Contact, type IntegrationPublic, type Prompt, type Tag, type Workflow, type WorkflowTrigger } from '../tauri';
import { confirm } from '../lib/confirm';
import { useTags } from '../lib/useTags';
import { ConditionEditor } from '../components/workflows/ConditionEditor';
import { ActionStepsEditor } from '../components/workflows/ActionStepsEditor';
import { Toggle } from '../components/Toggle';
import { RowMenu } from '../components/RowMenu';
import { PageHeader } from '../components/PageHeader';
import { EmptyState } from '../components/EmptyState';
import { Select } from '../components/Select';

const TRIGGER_LABELS: Record<WorkflowTrigger, string> = {
  finalized: 'Recording finalized',
  deleted: 'Recording deleted',
  imported: 'Session imported',
  finalize_failed: 'Processing failed',
};

function freshWorkflow(): Workflow {
  return {
    id: '',
    name: '',
    enabled: true,
    trigger: 'finalized',
    condition: { type: 'all', children: [] },
    actions: [],
    created_at_unix_seconds: 0,
  };
}

function summarizeTrigger(w: Workflow): string {
  const conds = w.condition.type === 'all' || w.condition.type === 'any' ? w.condition.children.length : 1;
  const c = conds === 0 ? 'every recording' : `${conds} condition${conds === 1 ? '' : 's'}`;
  return `When ${TRIGGER_LABELS[w.trigger].toLowerCase()} · ${c} · ${w.actions.length} step${w.actions.length === 1 ? '' : 's'}`;
}

function hasRunPrompt(actions: ActionStep[]): boolean {
  return actions.some((a) => a.type === 'run_prompt');
}

function Editor({ initial, prompts, integrations, tags, contacts, onDone }: {
  initial: Workflow;
  prompts: Prompt[];
  integrations: IntegrationPublic[];
  tags: Tag[];
  contacts: Contact[];
  onDone: (saved: boolean) => void;
}) {
  const [name, setName] = useState(initial.name);
  const [trigger, setTrigger] = useState<WorkflowTrigger>(initial.trigger);
  const [condition, setCondition] = useState<ConditionNode>(initial.condition);
  const [actions, setActions] = useState<ActionStep[]>(initial.actions);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const promptOnWrongTrigger = trigger !== 'finalized' && hasRunPrompt(actions);
  const canSave = !busy && !promptOnWrongTrigger;

  const save = () => {
    setBusy(true);
    setError(null);
    tauri
      .workflowUpsert({ ...initial, name, trigger, condition, actions })
      .then(() => onDone(true))
      .catch((e) => { setError(String(e)); setBusy(false); });
  };

  return (
    <div className="editor-card" style={{ maxWidth: 720 }}>
      <div className="editor-card__title">{initial.id ? `Edit “${initial.name}”` : 'New workflow'}</div>
      <div style={{ marginTop: 8 }}>
        <label className="meta" style={{ fontSize: 'var(--t-ui)' }} htmlFor="wf-name">Name</label>
        <input data-testid="wf-name" id="wf-name" aria-label="Name" style={{ display: 'block', width: '100%', marginTop: 4 }} type="text" value={name} placeholder="e.g. Client A design specs" onChange={(e) => setName(e.target.value)} />
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 'var(--t-ui)' }} htmlFor="wf-when">When</label>
        <Select
          testId="wf-trigger"
          id="wf-when"
          label="When"
          className="wf-trigger"
          value={trigger}
          onChange={(v) => setTrigger(v as WorkflowTrigger)}
          options={(Object.keys(TRIGGER_LABELS) as WorkflowTrigger[])
            .map((t) => ({ value: t, label: TRIGGER_LABELS[t] }))}
        />
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 'var(--t-ui)' }}>If</label>
        <div style={{ marginTop: 4 }}>
          <ConditionEditor node={condition} onChange={setCondition} tags={tags} contacts={contacts} />
        </div>
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 'var(--t-ui)' }}>Do</label>
        <div style={{ marginTop: 4 }}>
          <ActionStepsEditor steps={actions} onChange={setActions} trigger={trigger} prompts={prompts} integrations={integrations} />
        </div>
      </div>

      {promptOnWrongTrigger && (
        <p className="meta meta--danger">
          Run-prompt steps need the Recording-finalized trigger — remove them or switch back.
        </p>
      )}
      {error && <p className="meta meta--danger">{error}</p>}

      <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
        <button data-testid="wf-save" className="btn btn--primary" disabled={!canSave} onClick={save}>Save</button>
        <button data-testid="wf-cancel" className="btn" disabled={busy} onClick={() => onDone(false)}>Cancel</button>
      </div>
    </div>
  );
}

function Row({ w, onToggle, onEdit, onDelete }: {
  w: Workflow;
  onToggle: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <div className="list-row">
      <div style={{ flex: 1, minWidth: 0 }}>
        <div className="list-row__name">{w.name}</div>
        <div className="meta" style={{ fontSize: 'var(--t-small)' }}>
          {summarizeTrigger(w)}
        </div>
      </div>
      <Toggle testId={`wf-enabled-${w.id}`} label={`Enable ${w.name}`} checked={w.enabled} onChange={onToggle} />
      <button data-testid={`wf-edit-${w.id}`} className="btn" onClick={onEdit}>Edit</button>
      <RowMenu
        testId={`wf-menu-${w.id}`}
        label={`Actions for ${w.name}`}
        items={[{
          label: 'Delete',
          testId: `wf-delete-${w.id}`,
          danger: true,
          onSelect: onDelete,
        }]}
      />
    </div>
  );
}

export function Workflows() {
  const [workflows, setWorkflows] = useState<Workflow[] | null>(null);
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [integrations, setIntegrations] = useState<IntegrationPublic[]>([]);
  const tags = useTags();
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [editing, setEditing] = useState<Workflow | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    setError(null);
    Promise.all([
      tauri.workflowsList(),
      tauri.listPrompts(),
      tauri.listIntegrations(),
      tauri.listContacts(),
    ])
      .then(([w, p, i, c]) => {
        setWorkflows(w);
        setPrompts(p);
        setIntegrations(i);
        setContacts(c);
      })
      .catch((e) => setError(String(e)));
  };
  useEffect(() => { reload(); }, []);

  const toggle = (w: Workflow) => {
    void tauri.workflowUpsert({ ...w, enabled: !w.enabled }).then(reload).catch((e) => setError(String(e)));
  };
  const remove = (w: Workflow) => {
    void confirm({ title: `Delete "${w.name}"?`, body: 'The workflow is removed; past runs stay in History.' })
      .then((ok) => { if (ok) return tauri.workflowDelete(w.id).then(reload); })
      .catch((e) => setError(String(e)));
  };

  const active = (workflows ?? []).filter((w) => w.enabled);
  const inactive = (workflows ?? []).filter((w) => !w.enabled);

  return (
    <div className="page">
      <PageHeader
        title="Workflows"
        subtitle="When something happens to a recording, run prompts or send it somewhere — automatically."
        actions={!editing ? <button data-testid="wf-new" className="btn" onClick={() => setEditing(freshWorkflow())}>New workflow</button> : undefined}
      />
      <div className="page__body page__measure">
      {error && <p className="meta meta--danger">{error}</p>}

      {editing ? (
        <div style={{ marginTop: 16 }}>
          <Editor
            initial={editing}
            prompts={prompts}
            integrations={integrations}
            tags={tags}
            contacts={contacts}
            onDone={(saved) => { setEditing(null); if (saved) reload(); }}
          />
        </div>
      ) : (
        <>
          {workflows && workflows.length === 0 && (
            <EmptyState
              testid="wf-empty"
              title="No workflows yet"
              action={(
                <button data-testid="wf-empty-new" className="btn btn--primary" onClick={() => setEditing(freshWorkflow())}>
                  New workflow
                </button>
              )}
            >
              A workflow watches for something happening to a recording and acts on it —
              “when a recording tagged Client A is finalized, run the design-spec prompt and
              send it to your webhook”.
            </EmptyState>
          )}
          {workflows && workflows.length > 0 && (
            <div style={{ marginTop: 16 }}>
              {[...active, ...inactive].map((w) => (
                <Row key={w.id} w={w} onToggle={() => toggle(w)} onEdit={() => setEditing(w)} onDelete={() => remove(w)} />
              ))}
            </div>
          )}
        </>
      )}
      </div>
    </div>
  );
}
