// Headless driver projecting the qaStore "Ask AI" state onto the toast stack.
// Gives the in-flight ask a presence outside the Search page: an "Asking AI…"
// working toast you can click to jump back to Search, with a Cancel action.
// Because the ask lives in the store (not Search's local state), it keeps
// running while you're on other pages — this toast is how you find your way
// back to it.
import { useEffect } from 'react';
import { useQa, cancelQuestion } from '../lib/qaStore';
import { pushToast, dismissToast } from '../lib/toastStore';

export const QA_TOAST_ID = 'qa-ask';

export function QaToast({ onOpenSearch }: { onOpenSearch: () => void }): null {
  const qa = useQa();
  useEffect(() => {
    switch (qa.status) {
      case 'asking':
        pushToast({
          id: QA_TOAST_ID,
          severity: 'working',
          title: 'Asking AI…',
          body: qa.query ?? undefined,
          onClick: onOpenSearch,
          actions: [{ label: 'Cancel', onClick: () => cancelQuestion() }],
        });
        break;
      case 'done':
        pushToast({
          id: QA_TOAST_ID,
          severity: 'done',
          title: 'Answer ready',
          body: qa.query ?? undefined,
          onClick: onOpenSearch,
          autoDismissMs: 8000,
          dismissible: true,
        });
        break;
      case 'error':
        pushToast({
          id: QA_TOAST_ID,
          severity: 'error',
          title: 'Ask failed',
          body: qa.error ?? undefined,
          onClick: onOpenSearch,
          autoDismissMs: 10000,
          dismissible: true,
        });
        break;
      case 'idle':
      case 'cancelled':
        dismissToast(QA_TOAST_ID);
        break;
    }
  }, [qa.status, qa.query, qa.error, onOpenSearch]);
  return null;
}
