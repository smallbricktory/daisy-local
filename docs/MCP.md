# Daisy MCP server (local)

Daisy can expose a local [MCP](https://modelcontextprotocol.io) server so MCP
clients on the same machine — Claude Code, Claude Desktop — can query your
meeting library. **Read-only. Loopback-only (127.0.0.1). Off by default.**

## Enable + connect

1. Settings → MCP → "Enable local MCP server".
2. Copy the shown command and run it in a terminal:
   `claude mcp add --transport http daisy http://127.0.0.1:32479/mcp --header "Authorization: Bearer <token>"`
3. In any Claude Code session: "list my recent meetings", "what did we decide
   about X?" — the `mcp__daisy__*` tools do the rest.

## Tools

### Read (always available)

| Tool | Does |
|---|---|
| `list_sessions` | Sessions newest-first: id, title, time, duration, artifacts, tag_ids |
| `list_tags` | User's tags (id, name, color, use_count) — resolves the `tag_ids` from `list_sessions` |
| `get_transcript` | Full speaker-labelled transcript (markdown) |
| `get_summary` | Generated summary markdown (TL;DR, action items, decisions) |
| `search_meetings` | Semantic search; returns scored excerpts with session ids + timestamps |

### Write (opt-in: Settings → MCP → "Allow importing meetings")

| Tool | Does |
|---|---|
| `import_session` | Create a NEW session from text — any of transcript / summary / notes markdown. Create-only; never overwrites. |

`import_session` arguments (all optional, but supply at least one of the three
text fields):

```json
{
  "title": "Weekly sync",
  "occurred_at": 1749513600,
  "transcript_md": "**Alice:** Let's ship Friday.\n\n**Bob:** Agreed.",
  "summary_md": "## TL;DR\nShipping Friday.\n\n## Action items\n- Bob: cut release",
  "notes_md": "raw pasted notes…",
  "tag_ids": []
}
```

- The text is plain markdown — no required structure. For a native look,
  transcripts use `**Name:** line` and summaries use `## TL;DR` / `## Action items`.
- `occurred_at` (unix seconds) sets the session's position in the library;
  omit it to use now.
- The session is audio-less and finalized. The transcript shows in the
  transcript tab and joins semantic search; Regen / Diarize stay hidden
  (nothing to re-derive).
- The tool is **not advertised** in `tools/list` unless write access is on.

## Security model

- Binds 127.0.0.1 only — unreachable from the network. (LAN access is a
  planned, separate opt-in.)
- Every request needs the bearer token. The token is stored **in the encrypted
  vault**, not a plaintext file — so it's encrypted at rest and only exists in
  memory while Daisy is unlocked (on Windows a token file would have no ACL
  protection). The token stops *other local processes* from reading your
  meetings; loopback traffic itself can't be sniffed without same-user/root
  access. Note: the client (e.g. Claude Code) stores its own copy of the token
  in its config in plaintext — that copy is outside Daisy's control.
- Because the token lives in the vault, a passphrase-mode server only starts
  **after** you unlock; until then Settings shows an "unlock to reveal" hint
  instead of the connect command. Machine-mode (trust-this-machine) vaults
  auto-unlock at launch, so the server starts on launch.
- While the vault is locked, every tool call is refused.
- "Regenerate token" (Settings → MCP, requires unlock) revokes all previously
  configured clients.
- **Writes are off by default** and gated behind a *separate* toggle. A
  read-only server can't fabricate or alter meetings; only enable write access
  if you want a client to import sessions. `import_session` is create-only —
  it can add sessions but never overwrite or delete existing ones. Imported
  transcripts/summaries later feed AI features (Q&A, summaries), so only enable
  write access for clients you trust not to inject hostile content.

## Troubleshooting

- **"Not running" after enabling** — port 32479 taken. Set `"mcp_port"` in
  settings.json to a free port, toggle off/on (or restart). Reconnect clients
  with the new URL.
- **Tool calls fail with "vault is locked"** — unlock Daisy.
- **401s after regenerating the token** — re-run `claude mcp remove daisy`
  then the new `claude mcp add …` command from Settings.
- **WSL** — a server on Windows is reachable from inside WSL2 via the host
  loopback forwarding; the reverse (Daisy in WSL, client on Windows) also
  rides localhost forwarding. If it doesn't, check `networkingMode` in
  `.wslconfig`.
