import { useEffect, useState } from 'react';
import { copyToClipboard, tauri, type McpStatus, type Settings as SettingsT } from '../tauri';

/** Settings → MCP: expose the meeting library to local MCP clients
 *  (Claude Code, Claude Desktop). Loopback-only HTTP + bearer token —
 *  see docs/MCP.md for the security model. */
export function McpSettings({
  settings, update,
}: {
  settings: SettingsT;
  update: <K extends keyof SettingsT>(key: K, value: SettingsT[K]) => Promise<void>;
}) {
  const [status, setStatus] = useState<McpStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Local edit buffer for the port input — committed (validated + saved) on blur
  // / Enter, not on every keystroke. Kept in sync if the setting changes elsewhere.
  const [portInput, setPortInput] = useState(String(settings.mcp_port));

  useEffect(() => {
    tauri.mcpStatus().then(setStatus).catch((e: unknown) => setError(errText(e)));
  }, []);

  useEffect(() => { setPortInput(String(settings.mcp_port)); }, [settings.mcp_port]);

  async function commitPort() {
    const n = Number(portInput);
    if (!Number.isInteger(n) || n < 1024 || n > 65535) {
      setError('Port must be a whole number from 1024 to 65535.');
      setPortInput(String(settings.mcp_port));
      return;
    }
    if (n === settings.mcp_port) { setError(null); return; }
    setBusy(true);
    setError(null);
    try {
      // Checks availability before saving; a clash is caught up front, not
      // only when the server tries to bind.
      if (!(await tauri.mcpPortAvailable(n))) {
        setError(`Port ${n} is already in use — pick another.`);
        setPortInput(String(settings.mcp_port));
        return;
      }
      await update('mcp_port', n);
      // Rebind onto the new port if the server is running.
      if (settings.mcp_enabled) setStatus(await tauri.mcpApply());
    } catch (e: unknown) {
      setError(errText(e));
      setPortInput(String(settings.mcp_port));
    } finally {
      setBusy(false);
    }
  }

  async function setEnabled(on: boolean) {
    setBusy(true);
    setError(null);
    try {
      await update('mcp_enabled', on);
      setStatus(await tauri.mcpApply());
    } catch (e: unknown) {
      setError(errText(e)); // e.g. port already in use — surfaced from the bind
    } finally {
      setBusy(false);
    }
  }

  async function regenerate() {
    setBusy(true);
    setError(null);
    try {
      setStatus(await tauri.mcpRegenerateToken());
    } catch (e: unknown) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  }

  async function copyCommand() {
    if (!status) return;
    await copyToClipboard(status.claude_command);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <section>
      <h2 className="h2">MCP server</h2>
      <p className="meta" style={{ maxWidth: 560 }}>
        Lets MCP clients on this computer (Claude Code, Claude Desktop) read your
        meeting library: list sessions, fetch transcripts and summaries, semantic
        search. Read-only and loopback-only — never reachable from the network.
        Queries are refused while the vault is locked.
      </p>

      <Field label="Local MCP server">
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 14 }}>
          <input
            type="checkbox"
            checked={!!settings.mcp_enabled}
            disabled={busy}
            onChange={(e) => void setEnabled(e.target.checked)}
          />
          <span>Enable</span>
        </label>
        {error && <p className="meta" style={{ color: 'var(--danger, #c00)' }}>{error}</p>}
      </Field>

      <Field label="Port">
        <input
          type="number"
          min={1024}
          max={65535}
          value={portInput}
          disabled={busy}
          onChange={(e) => setPortInput(e.target.value)}
          onBlur={() => void commitPort()}
          onKeyDown={(e) => { if (e.key === 'Enter') e.currentTarget.blur(); }}
          style={{ width: 120 }}
        />
        <p className="meta" style={{ maxWidth: 560 }}>
          Loopback port for the MCP server (default 32479). Change it if another
          app already uses this one — the new port is checked for availability
          before it's saved.
        </p>
      </Field>

      {settings.mcp_enabled && status && (
        <>
          <Field label="Status">
            <p className="meta" style={{ margin: 0 }}>
              {status.running
                ? `Running at ${status.endpoint}`
                : 'Not running — check the log (port conflict?).'}
            </p>
          </Field>

          {!status.claude_command && (
            <p className="meta" style={{ maxWidth: 560 }}>
              Unlock the vault to reveal the connect command — the token is
              stored encrypted and is only available while Daisy is unlocked.
            </p>
          )}

          {status.claude_command && (
          <Field label="Connect Claude Code">
            <pre
              style={{
                fontSize: 12, padding: '10px 12px', borderRadius: 6,
                border: '1px solid var(--frost-deep)', overflowX: 'auto',
                userSelect: 'all', whiteSpace: 'pre-wrap', wordBreak: 'break-all',
                maxWidth: 560, margin: 0,
              }}
            >
              {status.claude_command}
            </pre>
            <div style={{ display: 'flex', gap: 8, marginTop: 8 }}>
              <button type="button" className="btn" onClick={() => void copyCommand()}>
                {copied ? 'Copied ✓' : 'Copy command'}
              </button>
              <button type="button" className="btn" disabled={busy} onClick={() => void regenerate()}>
                Regenerate token
              </button>
            </div>
            <p className="meta" style={{ maxWidth: 560 }}>
              The command embeds a secret token — treat it like a password.
              Regenerating invalidates previously configured clients.
            </p>
          </Field>
          )}

          <Field label="Write access">
            <label style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 14 }}>
              <input
                type="checkbox"
                checked={!!settings.mcp_allow_write}
                disabled={busy}
                onChange={(e) => void update('mcp_allow_write', e.target.checked)}
              />
              <span>Allow importing meetings (text only)</span>
            </label>
            <p className="meta" style={{ maxWidth: 560 }}>
              Adds an <code>import_session</code> tool so a client can create new
              sessions from transcript / summary / notes text. Create-only — it
              never overwrites or deletes. Leave off if you only want read access.
            </p>
          </Field>
        </>
      )}
    </section>
  );
}

function errText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: 16 }}>
      <label style={{ display: 'block', fontSize: 13, color: 'var(--muted)', marginBottom: 4 }}>{label}</label>
      {children}
    </div>
  );
}
