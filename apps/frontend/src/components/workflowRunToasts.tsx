import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import { pushToast } from '../lib/toastStore';
import type { WorkflowRunEvent } from '../tauri';

/** Projects workflow:run events onto one toast per run (stable id =
 *  workflow:<run_id>) — same reuse-the-id pattern as sessionStatusToasts. */
export function WorkflowRunToasts({ onOpenHistory }: { onOpenHistory: () => void }) {
  useEffect(() => {
    const un = listen<WorkflowRunEvent>('workflow:run', ({ payload: p }) => {
      const id = `workflow:${p.run_id}`;
      if (p.status === 'running') {
        pushToast({
          id,
          severity: 'working',
          title: p.workflow_name,
          body: p.step_label ? `step ${p.step_index + 1} of ${p.step_count}: ${p.step_label}` : 'starting…',
          progress: p.step_count > 0 ? p.step_index / p.step_count : undefined,
        });
      } else if (p.status === 'ok') {
        pushToast({
          id,
          severity: 'done',
          title: p.workflow_name,
          body: 'Workflow completed.',
          autoDismissMs: 8000,
          onClick: onOpenHistory,
        });
      } else {
        pushToast({
          id,
          severity: p.status === 'partial' ? 'warning' : 'error',
          title: p.workflow_name,
          body: p.status === 'partial'
            ? 'Finished with some failed steps — click for History.'
            : 'Workflow failed — click for History.',
          dismissible: true,
          onClick: onOpenHistory,
        });
      }
    });
    return () => { void un.then((f) => f()); };
  }, [onOpenHistory]);
  return null;
}
