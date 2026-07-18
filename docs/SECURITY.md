# Daisy — Security & Threat Model

Snapshot 2026-07-07, repo `smbr-daisy-app` @ `6dbf82b`. Every claim below is
sourced from the code at that commit; file references are given so it can be
re-audited.

Daisy is a **local-first** meeting recorder, transcriber, and summarizer.
The security posture follows from that: your recordings, transcripts, and
summaries live on your disk, in open formats, and leave your machine only
through paths you explicitly configure. Credentials are encrypted at rest.
The app is fully functional offline.

---

## 1. Architecture and trust boundaries

```
┌─ Your machine ──────────────────────────────────────────────┐
│                                                             │
│  ┌─ Daisy app (Tauri) ───────────────────────────┐          │
│  │  WebView UI ── strict CSP: no network access  │          │
│  │  Rust core ── all egress happens here         │          │
│  └───────────────┬───────────────────────────────┘          │
│                  │                                          │
│  ┌─ Profile directory ─────────────────────────┐            │
│  │  keys.vault.json      ENCRYPTED (secrets)   │            │
│  │  calendar/events.json ENCRYPTED             │            │
│  │  sessions/<id>/…      plaintext (content)   │            │
│  │  *.json config        plaintext (no secrets)│            │
│  └─────────────────────────────────────────────┘            │
│                                                             │
│  Loopback only: MCP server 127.0.0.1 (off by default)       │
│  Optional local AI: Ollama :11434 / LM Studio :1234         │
└──────────────┬──────────────────────────────────────────────┘
               │ TLS, only when configured/triggered
               ▼
   daisy.smbr.app (license, updates, optional Daisy Cloud)
   api.anthropic.com / api.openai.com / api.groq.com (optional cloud LLM)
   huggingface.co (model downloads, user-initiated)
   user-supplied URLs (calendar feeds, webhooks)
```

Trust boundaries:

1. **WebView ↔ Rust core.** The UI cannot reach the network or the
   filesystem directly; every operation goes through Tauri commands. CSP:
   `connect-src ipc: http://ipc.localhost` only — even injected script in the
   WebView has no network path (`tauri.conf.json`).
2. **App ↔ disk.** Secrets cross this boundary only inside the encrypted
   vault envelope. Meeting content crosses it in plaintext by design (§3).
3. **Machine ↔ network.** Nothing containing meeting content crosses this
   boundary unless you configure a cloud provider, the Daisy Cloud gateway,
   or a webhook integration (§5).

## 2. Data classification

| Class | Examples | At rest | Rationale |
|---|---|---|---|
| **Credentials** | Provider API keys, webhook auth secrets, calendar feed URLs, install signing key, MCP token, voiceprint embeddings | **Encrypted** (vault) | Dangerous in motion; encryption must travel with the file (backups, sync) |
| **Meeting content** | Audio, transcripts, summaries, chapters, in-call chat, search index | Plaintext, open formats | Your data; grep-able, sync-able, outlives the app. Protect with full-disk encryption (§7) |
| **Configuration** | Settings, tags, prompts, workflows, contacts | Plaintext | Needed before vault unlock; contains no secrets |
| **License/install identity** | Install ID, license key, signed validity stamp | Plaintext | Needed before vault unlock; low-value, seat-limited, server-revocable |

## 3. Data inventory (at rest)

All paths relative to the profile directory (platform data dir, e.g.
`~/.local/share/…` on Linux) unless noted.

### Encrypted — vault envelope

| Artifact | Contents |
|---|---|
| `keys.vault.json` | Provider API keys, per-provider config, integration webhook auth (header/bearer values), subscribed calendar feed URLs, per-install Ed25519 signing seed, MCP bearer token, voiceprint embeddings |
| `calendar/events.json` | Parsed calendar event cache (derived from secret feed URLs; re-fetchable, so passphrase loss loses nothing) |

Vault cryptography (`crates/vault`):

- **AES-256-GCM**, key derived with **Argon2id v1.3** (64 MiB memory,
  3 iterations, 4 lanes). Fresh random 16-byte salt and 12-byte nonce on
  every write. Wrong passphrase = authentication failure, not garbage output.
- **Passphrase mode** (default): minimum 22 characters *and* zxcvbn score
  ≥ 3 ("strong") enforced at creation. **There is no recovery** — a lost
  passphrase means re-entering your keys, never lost recordings.
- **Machine mode** (opt-in convenience): the key is derived from the OS
  machine ID (`/etc/machine-id`, Windows `MachineGuid`, macOS
  `IOPlatformUUID`) and the vault auto-unlocks at startup. This protects
  vault copies that leave the machine (backups, synced folders) but **not**
  against anyone with access to the running machine. Choose passphrase mode
  if your threat model includes other local users.
- While the app runs the vault is unlocked and keys are held in memory
  (zeroized wrappers, `crates/tauri-app/src/state.rs`). The app cannot be
  used with a locked vault; a manual lock is available in Settings.

### Plaintext — meeting content (sessions/<id>/)

| Artifact | Contents |
|---|---|
| audio files | Raw recorded audio (mic and system tracks) |
| `manifest.json` | Session metadata: title, times, participants, tags |
| `transcript.json`, `transcript.dedup.json` | Full transcript with timestamps and speakers |
| `summary.json`, `chapters.json` | Generated summaries and chapters |
| `call-chat.json` | In-call Q&A thread |
| `chunks.json`, `embeddings.bin` | Local semantic-search index over the transcript |
| `finalize.status.json`, `finalize.recovery.json` | Processing state/recovery markers |

### Plaintext — configuration (profile root)

`settings.json` (preferences only — the file's own header documents "secrets
go in the vault"), `tags.json`, `contacts.json` (names/emails you add),
`prompts.json`, `workflows.json`, `workflow_queue.json`,
`integration_history.json` (which meeting was pushed where — metadata only),
`binding.json` (profile-to-install binding; contains a SHA-256 MAC, no key
material), `bootstrap.json`, `eula.json`, `consent.json`, `migrations.json`.

### Plaintext — machine config (platform config dir)

`install.json`: install ID, a random per-install binding key, trial dates,
the license key, and the license validity stamp (an Ed25519-signed payload
from the license server; signature verified against a pinned vendor public
key, `commands/license.rs`). Kept outside the vault because it is needed
before unlock. The license key is deliberately classed low-value: seat-
limited (3 devices) and revocable server-side.

### Logs

Local only, `<profile>/logs/`, rotated daily, deleted after 7 days.
Transcript and summary text is never written to logs. Performance lines
that reference application window names use salted hashes, not the names
(`crates/tauri-app/src/perf.rs`).

**Telemetry: none.** The module named `telemetry.rs` samples the app's own
CPU/RSS into the local log; nothing is transmitted. There are no analytics,
crash reporters, or tracking calls anywhere in the app.

## 4. What leaves your machine, and when

Offline, Daisy records, transcribes (local Whisper), summarizes (local LLM
via Ollama/LM Studio), and searches with zero egress. This section is the
complete egress inventory — a full-repo sweep of all Rust crates, the
frontend, configs, and CI; the two automatic calls were verified by reading
the gating code, not just searching for URLs.

### Automatic (no meeting content, ever)

| Call | Destination | Carries | Gate | Code |
|---|---|---|---|---|
| Update check | `daisy.smbr.app/updates/{os}/latest.json` | Version string in user-agent | On launch + every 6 h, **skipped unless the auto-update-check setting is on**; **notify-only** — the app never downloads or installs updates itself; 10 s timeout, fail-silent | `commands/update.rs`, `App.tsx` |
| License check-in | `daisy.smbr.app/api/license/refresh` | License key, install ID | On launch + 6 h beat; backend throttles to ~1/day (`last_checkin_unix`); 8 s timeout, fail-silent | `commands/license.rs` |
| Calendar refresh | Your ICS/webcal URLs | The fetch itself | Exists only if you subscribed a calendar; 30 s timeout | `commands/calendar.rs` |

### User-configured or user-clicked (meeting content only by your choice)

| Feature | Destination | Carries | Notes | Code |
|---|---|---|---|---|
| Cloud summaries / chat / analysis | `api.anthropic.com/v1`; any OpenAI-compatible `/chat/completions` (incl. local LM Studio `:1234`, Ollama `:11434`) | **Transcript text + your prompts** (your key) | User-selected provider; 180 s timeout | `summarize/chat.rs` |
| Daisy Cloud (managed AI) | `daisy.smbr.app/api/gateway/v1/chat/completions` | **Transcript text** — never audio | Ed25519-signed per install (timestamp + nonce + body hash); task-routed (`X-Daisy-Task`); no API key exists to steal | `summarize/gateway.rs` |
| License activate / deactivate | `daisy.smbr.app/api/activate`, `/api/deactivate` | License key, install ID, install public key | Only when you enter/remove a key | `commands/license.rs` |
| Webhook integrations | Your URL (Zapier/n8n/localhost/…) | The payloads you select per integration (summary / transcript / metadata) | Also used by workflow actions — same code path; 15 s timeout, 2 attempts (retry once on 429/5xx) | `commands/integrations.rs`, `commands/workflow_engine.rs` |
| Calendar subscription | Your ICS URL | Outbound: the fetch itself. Inbound: event data | | `commands/calendar.rs` |
| Whisper/LLM model download | `huggingface.co/ggerganov/whisper.cpp/…` | Nothing personal | User clicks download; 30 s connect / 30 min total, 3 retries | `providers-local/download.rs` |
| Help / changelog / subscribe / source links | `www.daisylocal.app/…`, `github.com/smallbricktory/daisy-local` | Nothing — opens in your OS browser | User click only; the open-external command allows `http(s)` URLs only | `App.tsx`, `Settings.tsx` |

Every remote call uses TLS. The only plaintext HTTP in the entire app is
loopback (local AI servers, the MCP server).

### Local listeners (inbound, loopback only, never remote)

| What | Address | Gate |
|---|---|---|
| MCP server (lets tools like Claude Desktop query your meetings) | `http://127.0.0.1:<port>/mcp` | Off by default; authenticated with a 256-bit bearer token stored in the vault, compared timing-safe; write tools require a second, separate opt-in setting (`crates/tauri-app/src/mcp/`) |
| Vite dev probe | `127.0.0.1:5173` | Debug builds only; not present in releases |

### The UI cannot talk to the network

The frontend performs **zero direct HTTP** — every network operation above
happens in the Rust core behind an explicit command. Fonts are self-hosted
(no Google Fonts fetch); there are no external scripts, styles, iframes, or
remote images. Combined with the CSP (§1), even injected script in the
WebView has no network path.

### Build-time only (developer machines / CI — never ships in the app)

Toolchain and model pulls (`sh.rustup.rs`, NuGet, Hugging Face model
snapshots, GitHub release assets for the AEC model), release upload to S3
via GitHub OIDC. Test suites use local mocks; no live network in tests.

## 5. Threat model

### Assumptions

- The OS and your user account are not compromised. Daisy is an application,
  not an anti-malware boundary: software running as your user can read what
  you can read, including memory of a running process.
- TLS/PKI works: certificate validation is the transport integrity boundary.

### Threats addressed

| Threat (STRIDE) | Scenario | Mitigation |
|---|---|---|
| Information disclosure | Profile folder leaves the machine: cloud backup, Syncthing, sold laptop, support zip | Credentials remain AES-256-GCM ciphertext wherever the files travel. Meeting content is plaintext by design — see residual risks |
| Information disclosure | Another local user / admin reads the vault file | Passphrase mode: Argon2id makes offline guessing expensive; strength policy blocks weak passphrases |
| Information disclosure | Meeting data leaks into diagnostics | No telemetry; logs carry no transcript text; window names hashed |
| Spoofing | Stolen API-key-style credential for Daisy Cloud | No bearer credential exists: per-install Ed25519 signature over method, path, timestamp, nonce, and body hash; replay limited by timestamp + nonce |
| Tampering | Malicious "update" | Updater is notify-only — it can only tell you a version exists; you download from the website through your browser. Windows builds are Authenticode-signed; macOS builds are Developer ID-signed |
| Tampering | License forgery | Validity stamps are Ed25519-signed by the vendor and verified against a public key pinned in the binary |
| Elevation of privilege | Web content escapes the UI | Strict CSP (no network, no frames, no objects); frontend performs zero direct HTTP; all privileged operations are explicit Tauri commands |
| Elevation of privilege | Local process abuses the MCP port | Loopback bind + vault-held bearer token; server off by default; write access separately gated |
| Tampering | Path escape via session IDs | Session paths are sanitized to plain names under the sessions dir (`profile.rs`) |

### Explicitly out of scope

- **Compromised OS / root-level malware / memory scraping** of the running,
  unlocked app. No desktop application can defend this.
- **Machine-mode vault against a local attacker** — documented tradeoff (§3).
- **Cloud provider data handling.** If you configure Groq/OpenAI/etc.,
  their retention policies apply to what you send them. Daisy's default,
  local processing, sends nothing.
- **Physical theft of a powered-on, logged-in machine.**

## 6. Residual risks

1. **Meeting content is plaintext on disk.** Deliberate: encrypting it would
   make a lost passphrase destroy your entire meeting history (the vault has
   no recovery), complicate crash recovery of in-progress recordings, and
   break greping/syncing your own data — while adding little against the
   realistic attacker (anything running as your user reads it anyway while
   the app is open). The mitigation is full-disk encryption (§7), which
   covers the actual at-rest threat: a stolen or discarded disk.
2. **The vault protects copies, not the running machine.** An attacker with
   persistent code execution as your user can wait for unlock. The vault's
   real value is at-rest and in-motion protection of the file.
3. **Model downloads are not checksum-pinned.** Whisper/LLM model files are
   fetched over TLS from Hugging Face; integrity rests on TLS and the host.
4. **Webhooks trust your endpoint.** Payloads you select are POSTed to the
   URL you configure; a mistyped or hostile endpoint receives them.

## 7. What you should do

- **Turn on full-disk encryption** — BitLocker (Windows), FileVault (macOS),
  LUKS (Linux). This is the intended protection for meeting content at rest.
- **Prefer passphrase mode** for the vault on shared or managed machines.
- **Treat the profile folder like the recordings it contains** when backing
  up or syncing: the vault file is safe anywhere; transcripts are as private
  as the folder you put them in.
- Point webhooks only at endpoints you control or trust.

## 8. Reporting a vulnerability

Please report suspected vulnerabilities privately by email (see the contact
address published on the website) rather than in a public issue. Include
steps to reproduce and the build SHA from Settings → About. We aim to
acknowledge within 72 hours.
