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
      role="dialog"
      aria-modal="true"
      onClick={() => setOpen(false)}
      style={{
        position: 'fixed', inset: 0, background: 'rgba(0,0,0,0.45)',
        display: 'flex', alignItems: 'center', justifyContent: 'center', zIndex: 1000,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: 'var(--bg, #fff)', color: 'var(--fg, #1a1a1a)',
          borderRadius: 12, padding: '24px 28px', maxWidth: 420, width: '90%',
          boxShadow: '0 10px 40px rgba(0,0,0,0.25)',
        }}
      >
        <h2 className="h2" style={{ margin: '0 0 8px' }}>Daisy Cloud</h2>
        <p style={{ margin: '0 0 18px', lineHeight: 1.5, opacity: 0.85 }}>
          Daisy Cloud is for internal use only. For more information, see{' '}
          <a
            href={FAQ_URL}
            onClick={(e) => { e.preventDefault(); void tauri.openExternal(FAQ_URL); }}
          >
            {FAQ_URL}
          </a>.
        </p>
        <div style={{ display: 'flex', gap: 10, justifyContent: 'flex-end' }}>
          <button className="btn" onClick={() => setOpen(false)}>Close</button>
        </div>
      </div>
    </div>
  );
}
