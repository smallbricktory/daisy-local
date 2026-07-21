import { useEffect, useState } from 'react';
import { tauri, type ActionStep, type ConditionNode, type Contact, type IntegrationPublic, type Prompt, type Tag, type Workflow, type WorkflowTrigger } from '../tauri';
import { confirm } from '../lib/confirm';
import { ConditionEditor } from '../components/workflows/ConditionEditor';
import { ActionStepsEditor } from '../components/workflows/ActionStepsEditor';


const TRIGGER_COLORS: Record<WorkflowTrigger, string> = {
  finalized: 'var(--indigo)',
  imported: 'var(--marigold-deep)',
  deleted: 'var(--iron)',
  finalize_failed: 'var(--danger)',
};

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
    <div style={{ padding: 'var(--space-3) var(--space-2)', borderBottom: 'var(--rule)', background: 'var(--tint)', maxWidth: 720 }}>
      <div style={{ fontWeight: 600, marginBottom: 8 }}>{initial.id ? `Edit “${initial.name}”` : 'New workflow'}</div>
      <div style={{ marginTop: 8 }}>
        <label className="meta" style={{ fontSize: 13 }} htmlFor="wf-name">Name</label>
        <input id="wf-name" aria-label="Name" style={{ display: 'block', width: '100%', marginTop: 4 }} type="text" value={name} placeholder="e.g. Client A design specs" onChange={(e) => setName(e.target.value)} />
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }} htmlFor="wf-when">When</label>
        <select id="wf-when" aria-label="When" style={{ display: 'block', marginTop: 4, maxWidth: 420 }} value={trigger} onChange={(e) => setTrigger(e.target.value as WorkflowTrigger)}>
          {(Object.keys(TRIGGER_LABELS) as WorkflowTrigger[]).map((t) => (
            <option key={t} value={t}>{TRIGGER_LABELS[t]}</option>
          ))}
        </select>
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }}>If</label>
        <div style={{ marginTop: 4 }}>
          <ConditionEditor node={condition} onChange={setCondition} tags={tags} contacts={contacts} />
        </div>
      </div>

      <div style={{ marginTop: 12 }}>
        <label className="meta" style={{ fontSize: 13 }}>Do</label>
        <div style={{ marginTop: 4 }}>
          <ActionStepsEditor steps={actions} onChange={setActions} trigger={trigger} prompts={prompts} integrations={integrations} />
        </div>
      </div>

      {promptOnWrongTrigger && (
        <p className="meta" style={{ color: 'var(--danger)' }}>
          Run-prompt steps need the Recording-finalized trigger — remove them or switch back.
        </p>
      )}
      {error && <p className="meta" style={{ color: 'var(--danger)' }}>{error}</p>}

      <div style={{ display: 'flex', gap: 8, marginTop: 16 }}>
        <button className="btn btn--primary" disabled={!canSave} onClick={save}>Save</button>
        <button className="btn" disabled={busy} onClick={() => onDone(false)}>Cancel</button>
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
    <div style={{ display: 'flex', alignItems: 'center', gap: 12, padding: 'var(--space-2) 0', borderBottom: 'var(--rule)' }}>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontWeight: 600 }}>{w.name}</div>
        <div className="meta" style={{ fontSize: 12, display: 'flex', alignItems: 'center', gap: 6 }}>
          <span aria-hidden="true" style={{ width: 8, height: 8, borderRadius: 999, background: TRIGGER_COLORS[w.trigger], opacity: w.enabled ? 1 : 0.45, flex: '0 0 auto' }} />
          {summarizeTrigger(w)}
        </div>
      </div>
      <label style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }} className="meta">
        <input type="checkbox" checked={w.enabled} onChange={onToggle} />
        Enabled
      </label>
      <button className="btn" onClick={onEdit}>Edit</button>
      <button className="btn btn--danger" onClick={onDelete}>Delete</button>
    </div>
  );
}

export function Workflows() {
  const [workflows, setWorkflows] = useState<Workflow[] | null>(null);
  const [prompts, setPrompts] = useState<Prompt[]>([]);
  const [integrations, setIntegrations] = useState<IntegrationPublic[]>([]);
  const [tags, setTags] = useState<Tag[]>([]);
  const [contacts, setContacts] = useState<Contact[]>([]);
  const [editing, setEditing] = useState<Workflow | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = () => {
    setError(null);
    Promise.all([
      tauri.workflowsList(),
      tauri.listPrompts(),
      tauri.listIntegrations(),
      tauri.listTags(),
      tauri.listContacts(),
    ])
      .then(([w, p, i, t, c]) => {
        setWorkflows(w);
        setPrompts(p);
        setIntegrations(i);
        setTags(t);
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
    <div style={{ padding: 'var(--space-4)', maxWidth: 880 }}>
      <div style={{ display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', gap: 12 }}>
        <h1 className="h1">Workflows</h1>
        {!editing && <button className="btn" onClick={() => setEditing(freshWorkflow())}>New workflow</button>}
      </div>
      <p className="meta" style={{ fontSize: 13, marginTop: 4 }}>
        When something happens to a recording, run prompts or send it somewhere — automatically.
      </p>

      {error && <p className="meta" style={{ color: 'var(--danger)', marginTop: 12 }}>{error}</p>}

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
            <p className="meta" style={{ marginTop: 24 }}>
              No workflows yet. Try “when a recording tagged Client A is finalized, run the design-spec prompt and send it to your webhook”.
            </p>
          )}
          {workflows && workflows.length > 0 && (
            <div style={{ marginTop: 16 }}>
              <h2 className="h2">Active</h2>
              {active.length === 0 && <p className="meta">None.</p>}
              {active.map((w) => (
                <Row key={w.id} w={w} onToggle={() => toggle(w)} onEdit={() => setEditing(w)} onDelete={() => remove(w)} />
              ))}
              <h2 className="h2">Inactive</h2>
              {inactive.length === 0 && <p className="meta">None.</p>}
              {inactive.map((w) => (
                <Row key={w.id} w={w} onToggle={() => toggle(w)} onEdit={() => setEditing(w)} onDelete={() => remove(w)} />
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
