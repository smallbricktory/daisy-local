import { ConfirmDialog } from './ConfirmDialog';
import { usePendingConfirm, resolvePending } from '../lib/confirm';

/** Single mount in App.tsx. Renders <ConfirmDialog/> when an imperative
 *  `confirm()` / `alert()` call from lib/confirm has a pending promise. */
export function GlobalConfirm() {
  const pending = usePendingConfirm();
  if (!pending) return null;
  const { spec } = pending;
  const hideCancel = spec.cancelLabel === '__hidden__';
  return (
    <ConfirmDialog
      title={spec.title}
      body={spec.body}
      confirmLabel={spec.confirmLabel ?? 'OK'}
      cancelLabel={hideCancel ? '' : spec.cancelLabel}
      danger={spec.danger}
      typedConfirm={spec.typedConfirm}
      elevated
      onCancel={() => resolvePending(false)}
      onConfirm={() => resolvePending(true)}
    />
  );
}
