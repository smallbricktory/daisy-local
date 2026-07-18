// Daisy Cloud not-entitled notice trigger. Backend AI commands reject with an
// AppError `{ kind, code, message, friendly }`; kind === 'GatewayNotEntitled'
// means the license isn't enabled for Daisy Cloud. We surface an informational
// dialog (GatewayNoticeModal listens for the window event) rather than a raw
// error.

export const GATEWAY_NOTICE_EVENT = 'daisy:gateway-notice';

/** True when an invoke() rejection is the gateway "not entitled" error. */
export function isGatewayNotEntitled(err: unknown): boolean {
  return !!err && typeof err === 'object' && (err as { kind?: string }).kind === 'GatewayNotEntitled';
}

/** Show the Daisy Cloud notice dialog. */
export function showGatewayNotice(): void {
  window.dispatchEvent(new CustomEvent(GATEWAY_NOTICE_EVENT));
}

/** If `err` is the gateway not-entitled error, show the notice and return true
 *  (caller should then stop surfacing the raw error). Otherwise false. */
export function showGatewayNoticeIfNeeded(err: unknown): boolean {
  if (isGatewayNotEntitled(err)) {
    showGatewayNotice();
    return true;
  }
  return false;
}
