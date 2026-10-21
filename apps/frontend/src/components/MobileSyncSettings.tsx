// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import QRCode from 'qrcode';
import { copyToClipboard, errStr, tauri, type FirewallHint, type PairedPhoneView, type RefusalReason, type SyncStatus } from '../tauri';
import { RowMenu } from './RowMenu';
import { confirm } from '../lib/confirm';
import { fmtDate, fmtDateTime } from '../lib/format';
import { refusalCause, refusalRemedy } from '../lib/syncRefused';
import { outOfDateText } from '../lib/syncGate';
import { useBackendEvent } from '../lib/useBackendEvent';
import { useNerdMode } from '../lib/nerdMode';
import { useVisibleInterval } from '../lib/useVisibleInterval';
import { Group } from './SettingsGroup';

/** What the reach line says about waking a device whose app is closed. */
const BLUETOOTH_NOTE: Record<SyncStatus['bluetooth'], string> = {
  on: ' Bluetooth is on, so a device syncs even when its app is closed.',
  off: ' Bluetooth is off, so a device syncs when you open Daisy on it. Turn Bluetooth on to sync in the background.',
  denied:
    ' Waking a device in the background needs Bluetooth permission: grant it under System Settings, Privacy & Security, Bluetooth. Until then a device syncs when you open Daisy on it.',
  unsupported: ' A device syncs when you open Daisy on it: waking a device in the background needs a Mac.',
};

/** Light margin around the code, in modules (ISO/IEC 18004). */
const QUIET = 4;
/** Rendered width of the QR, quiet zone included. */
const QR_PX = 270;

/** The QR as one inline SVG path from the module matrix: no data URI, no canvas.
 *  Black on white with its quiet zone in every theme. */
function Qr({ text }: { text: string }) {
  const { size, d } = useMemo(() => {
    const q = QRCode.create(text, { errorCorrectionLevel: 'M' });
    const size = q.modules.size;
    let d = '';
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) if (q.modules.get(y, x)) d += `M${x} ${y}h1v1h-1z`;
    }
    return { size, d };
  }, [text]);
  return (
    <svg
      data-testid="mobile-sync-qr"
      data-qr-text={text}
      viewBox={`${-QUIET} ${-QUIET} ${size + 2 * QUIET} ${size + 2 * QUIET}`}
      width={QR_PX}
      height={QR_PX}
      shapeRendering="crispEdges"
      role="img"
      aria-label="Pairing code"
    >
      <rect x={-QUIET} y={-QUIET} width={size + 2 * QUIET} height={size + 2 * QUIET} fill="#fff" />
      <path d={d} fill="#000" />
    </svg>
  );
}

/** Settings → Mobile Sync: this computer's pairing code and the phones
 *  paired to it. Identity and pairings are per machine. */
export function MobileSyncSettings() {
  const nerd = useNerdMode();
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [renaming, setRenaming] = useState<PairedPhoneView | null>(null);
  const [now, setNow] = useState(() => Math.floor(Date.now() / 1000));

  const refresh = useCallback(
    () => tauri.syncStatus().then((s) => { setStatus(s); setError(null); }).catch((e: unknown) => setError(errStr(e))),
    [],
  );
  useEffect(() => { void refresh(); }, [refresh]);

  const [hint, setHint] = useState<FirewallHint | null>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    tauri.linuxFirewallHint().then(setHint).catch(() => setHint(null));
  }, []);
  const copyHint = async () => {
    if (!hint) return;
    await copyToClipboard(hint.command);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  // Refreshes when the backend reports a pairing or hello.
  useBackendEvent('sync:changed', () => { void refresh(); });

  // One line per device and reason: "3 recordings from Rowan's phone are …".
  const refusedGroups = useMemo(() => {
    const groups = new Map<string, { device_id: string; reason: RefusalReason; count: number }>();
    for (const r of status?.refused ?? []) {
      const key = `${r.device_id}\u0000${r.reason}`;
      const g = groups.get(key) ?? { device_id: r.device_id, reason: r.reason, count: 0 };
      g.count += 1;
      groups.set(key, g);
    }
    return [...groups.values()];
  }, [status?.refused]);
  const phoneName = (deviceId: string) => status?.paired.find((p) => p.device_id === deviceId)?.name ?? 'a device';

  const win = status?.window ?? null;
  const expired = win !== null && win.expires_unix <= now;
  const open = win !== null && !expired ? win : null;
  // Ticks while a key shows, so the screen swaps to the expired state the
  // moment it expires.
  useVisibleInterval(() => setNow(Math.floor(Date.now() / 1000)), 1000, win !== null && !expired);

  // Leaving this screen ends the key, as Cancel does.
  const openRef = useRef(open);
  openRef.current = open;
  useEffect(() => () => {
    if (openRef.current) void tauri.syncPairingCancel();
  }, []);

  const run = async (op: () => Promise<void>) => {
    setBusy(true);
    try { await op(); } catch (e) { setError(errStr(e)); } finally { setBusy(false); }
  };
  const beginKey = () => run(async () => {
    const w = await tauri.syncPairingBegin();
    setNow(Math.floor(Date.now() / 1000));
    setStatus((s) => s && { ...s, window: w });
  });
  const cancel = () => run(async () => {
    await tauri.syncPairingCancel();
    setStatus((s) => s && { ...s, window: null });
  });
  const unpair = async (deviceId: string, name: string) => {
    const ok = await confirm({
      title: `Unpair ${name}?`,
      body: 'It stops syncing until it pairs again.',
      confirmLabel: 'Unpair',
      danger: true,
    });
    if (!ok) return;
    await run(async () => { setStatus(await tauri.syncUnpair(deviceId)); });
  };

  return (
    <>
      <Group title="Mobile Sync">
        <p className="meta" style={{ maxWidth: 560 }}>
          Pair a device running Daisy to sync its recordings to this computer. Recordings and
          notes move over your own network, encrypted with keys that stay on your devices. On the
          device, open Settings, Desktop Sync, and scan the QR, or type the key if the camera
          can&rsquo;t scan it. A device pairs with one computer; each computer has its own key.
        </p>
        {error && <p className="meta meta--danger" data-testid="mobile-sync-error">{error}</p>}
        {status && (
          <>
            {open ? (
              <div style={{ display: 'flex', gap: 20, alignItems: 'flex-start', marginTop: 16 }}>
                <Qr text={open.qr_text} />
                <div>
                  <pre className="command-block" data-testid="mobile-sync-key">{open.key_text}</pre>
                  <p className="meta meta--danger" data-testid="mobile-sync-warning" style={{ maxWidth: 320 }}>
                    Anyone who can see this key while it is showing can pair a device.
                  </p>
                  <button data-testid="mobile-sync-cancel" type="button" className="btn" disabled={busy} onClick={() => void cancel()}>
                    Cancel
                  </button>
                </div>
              </div>
            ) : expired ? (
              <div style={{ marginTop: 16 }}>
                <p className="meta" data-testid="mobile-sync-expired">This key expired. Show a new one?</p>
                <button
                  data-testid="mobile-sync-show-new"
                  type="button"
                  className="btn btn--primary"
                  disabled={busy}
                  onClick={() => void beginKey()}
                >
                  Show a new key
                </button>
              </div>
            ) : (
              <button
                data-testid="mobile-sync-show-code"
                type="button"
                className="btn btn--primary"
                disabled={busy}
                onClick={() => void beginKey()}
                style={{ marginTop: 16 }}
              >
                Pair a device
              </button>
            )}
            {status.platform === 'windows' && (
              <p className="meta" data-testid="mobile-sync-firewall" style={{ marginTop: 12, maxWidth: 560 }}>
                The first time this code shows, Windows asks whether to allow Daisy through its
                firewall. Choose Allow. After Cancel, devices stay blocked until Daisy is allowed
                under Windows Security, Firewall &amp; network protection, Allow an app through
                firewall.
              </p>
            )}
            {hint && (
              <div data-testid="mobile-sync-linux-firewall" style={{ marginTop: 12, maxWidth: 560 }}>
                <p className="meta">
                  Daisy requires ports {status.port} and 5353 to sync with local devices. Run the following command:
                </p>
                <pre className="command-block" data-testid="mobile-sync-linux-firewall-command">{hint.command}</pre>
                <button
                  data-testid="mobile-sync-linux-firewall-copy"
                  type="button"
                  className="btn"
                  style={{ marginTop: 8 }}
                  onClick={() => void copyHint()}
                >
                  {copied ? 'Copied ✓' : 'Copy command'}
                </button>
              </div>
            )}
            {nerd && status.paired.length > 0 && (
              <p className="meta" data-testid="mobile-sync-reach" style={{ marginTop: 16, maxWidth: 560 }}>
                {!status.listening
                  ? 'This computer is not accepting device connections right now.'
                  : status.host
                    ? `Devices reach this computer at ${status.host} on port ${status.port}.`
                    : 'Connect this computer to Wi-Fi or a network so devices can reach it.'}
                {BLUETOOTH_NOTE[status.bluetooth]}
              </p>
            )}
          </>
        )}
      </Group>
      {status && (
        <Group title="Paired devices">
          {status.paired.length === 0 ? (
            <p className="meta" data-testid="mobile-sync-empty">No device is paired with this computer.</p>
          ) : (
            <ul style={{ padding: 0, margin: 0 }}>
              {status.paired.map((p) => (
                <li
                  key={p.device_id}
                  data-testid={`mobile-sync-phone-${p.device_id}`}
                  style={{ display: 'flex', gap: 12, alignItems: 'center', padding: '8px 0' }}
                >
                  <div style={{ flex: 1 }}>
                    <div>{p.name}</div>
                    {p.phone_name !== p.name && (
                      <div className="meta" data-testid={`mobile-sync-paired-as-${p.device_id}`}>{p.phone_name}</div>
                    )}
                  </div>
                  {p.out_of_date ? (
                    <span className="meta meta--danger" data-testid={`mobile-sync-out-of-date-${p.device_id}`} style={{ maxWidth: 320 }}>
                      {outOfDateText(p.out_of_date, p.platform)}
                    </span>
                  ) : (
                    <span className="meta">
                      paired {fmtDate(p.paired_at_unix)}
                      {p.last_hello_unix ? ` · last seen ${fmtDateTime(p.last_hello_unix)}` : ''}
                    </span>
                  )}
                  <RowMenu
                    testId={`mobile-sync-menu-${p.device_id}`}
                    label={`Actions for ${p.name}`}
                    disabled={busy}
                    items={[
                      {
                        label: 'Rename',
                        testId: `mobile-sync-rename-${p.device_id}`,
                        onSelect: () => setRenaming(p),
                      },
                      {
                        label: 'Unpair',
                        testId: `mobile-sync-unpair-${p.device_id}`,
                        danger: true,
                        onSelect: () => void unpair(p.device_id, p.name),
                      },
                    ]}
                  />
                </li>
              ))}
            </ul>
          )}
          {refusedGroups.length > 0 && (
            <div data-testid="mobile-sync-refused" style={{ marginTop: 16, maxWidth: 560 }}>
              {refusedGroups.map((g) => (
                <p key={`${g.device_id}-${g.reason}`} className="meta meta--danger" data-testid={`mobile-sync-refused-${g.device_id}-${g.reason}`}>
                  {g.count === 1 ? 'One recording' : `${g.count} recordings`} from {phoneName(g.device_id)}{' '}
                  {g.count === 1 ? 'is' : 'are'} {refusalCause(g.reason)}. {refusalRemedy(g.reason, g.count)}
                </p>
              ))}
            </div>
          )}
        </Group>
      )}
      {renaming && (
        <PhoneRenameModal
          phone={renaming}
          onClose={() => setRenaming(null)}
          onSaved={(s) => { setStatus(s); setRenaming(null); }}
        />
      )}
    </>
  );
}

/** Names a paired phone on this computer; the name it paired with stays beneath. */
function PhoneRenameModal({
  phone, onClose, onSaved,
}: {
  phone: PairedPhoneView;
  onClose: () => void;
  onSaved: (s: SyncStatus) => void;
}) {
  const [name, setName] = useState(phone.name);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  async function save() {
    setBusy(true); setErr(null);
    try {
      onSaved(await tauri.syncRenameDevice(phone.device_id, name));
    } catch (e) {
      setErr(errStr(e));
      setBusy(false);
    }
  }
  return (
    <div data-testid="mobile-sync-rename-backdrop" className="modal-backdrop" onClick={busy ? undefined : onClose}>
      <div data-testid="mobile-sync-rename-modal" className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 420 }}>
        <h2 className="modal__title">Rename device</h2>
        <label className="field-label">Name on this computer</label>
        <input data-testid="mobile-sync-rename-name" type="text" autoFocus value={name} disabled={busy} maxLength={80}
               placeholder={phone.phone_name}
               onChange={(e) => setName(e.target.value)}
               onKeyDown={(e) => { if (e.key === 'Enter' && !busy) void save(); }}
               style={{ width: '100%', marginBottom: 12 }} />
        <p className="meta" style={{ marginBottom: 12 }}>It paired as {phone.phone_name}. Leave the name blank to use that.</p>
        {err && <p className="meta meta--danger">{err}</p>}
        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button data-testid="mobile-sync-rename-cancel" className="btn" onClick={onClose} disabled={busy}>Cancel</button>
          <button data-testid="mobile-sync-rename-save" className="btn btn--primary" onClick={() => void save()} disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>
    </div>
  );
}
