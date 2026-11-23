// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Daisy Cloud "not entitled" notice. Shown when an AI call returns the
// GatewayNotEntitled error (see lib/gatewayNotice). Daisy Cloud is for
// internal licenses only; this is a plain informational dialog with no
// pricing/contact CTAs. Listens for a window event; any error handler can
// trigger it without prop drilling.
import { useEffect, useState } from 'react';
import { tauri } from '../tauri';
import { GATEWAY_NOTICE_EVENT } from '../lib/gatewayNotice';

const FAQ_URL = 'https://www.daisylocal.app/faq#what-is-daisy-cloud';

export function GatewayNoticeModal() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onShow = () => setOpen(true);
    window.addEventListener(GATEWAY_NOTICE_EVENT, onShow);
    return () => window.removeEventListener(GATEWAY_NOTICE_EVENT, onShow);
  }, []);

  if (!open) return null;

  return (
    <div
      data-testid="gateway-notice-scrim"
      role="dialog"
      aria-modal="true"
      onClick={() => setOpen(false)}
      className="modal-backdrop"
    >
      <div
        data-testid="gateway-notice-modal"
        onClick={(e) => e.stopPropagation()}
        className="modal"
        style={{ width: 420 }}
      >
        <h2 className="modal__title">Daisy Cloud</h2>
        <p style={{ margin: '0 0 18px', lineHeight: 'var(--lh-text)' }}>
          Daisy Cloud is for internal use only. For more information, see{' '}
          <a
            data-testid="gateway-notice-faq"
            href={FAQ_URL}
            onClick={(e) => { e.preventDefault(); void tauri.openExternal(FAQ_URL); }}
          >
            {FAQ_URL}
          </a>.
        </p>
        <div style={{ display: 'flex', gap: 10, justifyContent: 'flex-end' }}>
          <button data-testid="gateway-notice-close" className="btn" onClick={() => setOpen(false)}>Close</button>
        </div>
      </div>
    </div>
  );
}
