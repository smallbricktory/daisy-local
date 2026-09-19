# UI test suite

Two modes, one suite, in `apps/frontend/e2e/`. The same spec files run against a
mocked backend in a browser and against the real binary on the desktop.

## Browser mode (seconds)

```sh
cd apps/frontend && npx pnpm@10 e2e:browser
```

Headless Chrome against the Vite dev server on port 5199, both started by the
wdio service. Every backend command is answered from `e2e/fixtures/world.ts` —
one fictional profile with sessions, transcripts, speakers, tags, workflows and
settings. CI runs it in the `linux` job, on every push to `main` and every PR.

`e2e/world-coverage.test.ts` keeps the world complete: `npx pnpm@10 test` fails
when `src/` invokes a command the world has no handler for.

Two more gates cover the suite itself, both in the `linux` job:

- `npx pnpm@10 run typecheck:e2e` typechecks `e2e/`. The root `tsconfig.json`
  includes `src` alone, and wdio runs the specs through `tsx`, which strips
  types without checking them.
- `npx pnpm@10 test` runs the testid ratchet and the world-coverage check.

## Native mode (after a build)

```sh
scripts/build-ui-test.sh                        # or scripts\build-ui-test.ps1
cd apps/frontend && npx pnpm@10 e2e:native
```

15 spec files against one app process — 22 s on an M-series mac. The release
`cargo tauri build` in front of it is what takes the minutes. CI runs both on the
Linux and Windows runners, in the `ui-tests` job, on every push.
The Windows runner is an interactive process (`run.cmd`) on the console
session, and that desktop must stay unlocked while the suite runs (see
Troubleshooting). `43-settings-mobile-sync` binds 0.0.0.0:7845 from the
ui-test binary, which raises the Windows firewall prompt the first time and
leaves it on the desktop until someone answers it; give the runner's binary
its rule once, from an elevated PowerShell, before the first run:

```powershell
New-NetFirewallRule -DisplayName "Daisy ui-test" -Direction Inbound -Action Allow `
  -Program "D:\c\release\daisy-app.exe" -Profile Any
```

The path is `$CARGO_TARGET_DIR\release\daisy-app.exe` as the `ui-tests` job
sets it.

The Windows runner shares the host, and with WSL's mirrored networking the
loopback, with the Linux runner, so the Windows leg drives its app on port
4545 and the Linux leg on 4445; on the default port for both, a suite that
overlapped the other's reached the other's app.

The build needs `cargo-tauri` (`scripts/setup-dev.sh` installs it;
`cargo install tauri-cli --version "^2" --locked` on a box that skipped it).

Three switches move together, and the script sets all three:

- `VITE_UI_TEST=1` builds the frontend into `apps/frontend/dist-ui-test`.
- `--features ui-test` compiles the wdio plugins in.
- `--config tauri.ui-test.conf.json` points `frontendDist` at `dist-ui-test` and
  turns on `withGlobalTauri`.

`crates/tauri-app/build.rs` widens the capability glob from
`./capabilities/*.json` to `./capabilities/**/*.json` under that same feature, so
the WebDriver ACL in `capabilities/ui-test/ui-test.json` reaches a ui-test binary
only. The output is `${CARGO_TARGET_DIR:-target}/release/daisy-app`.

The run launches that binary under a sandbox minted per run:
`wdio.native.conf.ts` creates a temp root and sets `DAISY_CONFIG_DIR` (EULA
record, consent, install id) and `DAISY_PROFILE_DIR` (sessions, user data, and
the key vault) inside it, plus the model directories. `DAISY_CONFIG_DIR` is
honored only by binaries built with the `ui-test` feature (and by `daisy-cli`)
— shipping app builds ignore it, so the install record that anchors the trial
cannot be redirected by an environment variable.

- `E2E_KEEP_PROFILE=1` keeps the sandbox after the run.
- `DAISY_E2E_BIN` points the run at another binary.

Linux runs under its own Xvfb, whatever display the host has:
`xvfb-run -a pnpm --dir apps/frontend run e2e:native`. On WSL, WSLg's X server
stalls the WebKit web process 30 s at a time (spec 21 took 7 min on `:0`, 3 s
under Xvfb) and can hang outright.
`scripts/setup-dev.sh` installs `xvfb`. The config sets `GDK_BACKEND=x11` (when
unset) so the app opens on that Xvfb display; GDK otherwise tries Wayland first
and reaches whatever holds `$XDG_RUNTIME_DIR/wayland-0`. The app reaches
PipeWire through `XDG_RUNTIME_DIR`; a runner started as a systemd service has
none, so the CI job exports `/run/user/<uid>` (and the session bus) before the
suite.

### One app, one document

Every spec file runs against the same app process and the same document, in
filename order. State carries across files: `21-session-detail` accounts for the
lifecycle toast `20-library`'s import leaves standing. Each spec's `before`
normalizes what it needs: `unlock.ensureMain(passphrase)` takes the app from
either the unlock screen or the main shell to the main shell. The invoke call
log is emptied per spec file (`clearCalls` in `wdio.shared.ts`).

## One spec

```sh
npx pnpm@10 e2e:native:spec e2e/specs/20-library.spec.ts
npx pnpm@10 e2e:browser:spec e2e/specs/20-library.spec.ts
```

pnpm forwards trailing arguments to the script. Write them bare; a `--`
separator is swallowed and the whole suite runs.

## Adding a screen

1. Give every interactive element a `data-testid` (`<screen>-<thing>`; rows
   `<screen>-row-<id>`). `npx pnpm@10 test` fails on any that is missing —
   `src/testid-coverage.test.ts` scans every `.tsx` under `src/`, at any depth,
   for a form control or an `onClick`/`onChange`/`onKeyDown`/`onSubmit` handler.
2. Add a page object in `e2e/pages/`. Selectors live there and nowhere else.
   `npx pnpm@10 test` fails on any id in `src/` that no page object or spec
   addresses — through `tid`/`tids`/`within`/`withinAll`/`typeInto`/
   `selectOption`/`expectText`, a `Scope`'s `one`/`many`, or a raw
   `[data-testid="…"]` / `[data-testid^="…"]` selector. A template id
   (`` `library-row-${id}` ``) is addressed by any reference sharing its
   literal head.
3. Write the spec in `e2e/specs/` as the user's path, asserting what they see.
4. New `invoke('x')`? Add `x` to `e2e/fixtures/world.ts` — `npx pnpm@10 test`
   fails until you do.

Two panes hold their controls inside a collapsed `<details>`: Settings →
Voiceprints ("Speaker recognition tuning") and Settings → Providers
("Transcription tuning"). The contents are in the document while it is shut and
have no box, so a read of them fails until it is open. Give the `<summary>` a
testid and click it first, as `vp-advanced` does.

Two things the ratchet does not see:

- `tagAttrs` walks to the first `>` outside braces, so a `>` inside an attribute
  string ends the attribute text early and a `data-testid` after it is missed.
- `EXCLUSIVE_IDS` allowlists one id written twice in a file on mutually
  exclusive branches, where only one is ever in the document.

## Mocking commands

`browser.tauri.mock()` patches `window.__TAURI__.core.invoke`. The app imports
`@tauri-apps/api` as a module, which reads `window.__TAURI_INTERNALS__.invoke`,
so that mock is inert here. Use `e2e/fixtures/invoke.ts`:

- `mockInvoke(cmd, value)` — resolve with a value. A cancelled file picker is
  `null`.
- `mockInvokeWith(cmd, impl)` — resolve from the args. `impl` is stringified into
  the page, so it must be closure-free and must return synchronously; it is
  probed once with `null` at install, which turns a leaked closure reference into
  a failure at the mock site.
- `mockInvokeReject(cmd, error)` — reject. The real backend rejects with its
  serialized `AppError` (`{ kind, code, message, friendly }`); mirror that shape
  when the UI reads fields off it.
- `callsOf(cmd)` — the args of every intercepted call, in order. Its length is
  the call count.
- `clearCalls()` — drop the log, keep the overrides.
- `restoreInvoke(cmd)` — hand the command back to the real backend.

The interceptor picks its seam per mode. Natively `__TAURI_INTERNALS__` and its
`invoke` are non-writable, and the custom-protocol IPC posts each command through
`window.fetch`, so it wraps `fetch` and matches on
`convertFileSrc(cmd, 'ipc')`. In browser mode the injected `invoke` is a plain
writable property and it wraps that. Installing throws when neither seam is
available.

Backend events go through `emitAppEvent(name, payload)` in
`e2e/fixtures/events.ts`: natively through `window.__TAURI__.event`, which the
ui-test overlay's `withGlobalTauri` exposes; in browser mode through the
service's event registry.

## Bridge notes

- The wdio plugins and their ACL ship in the `ui-test` binary alone.
- **Browser-mode boot.** `world.ts` is bundled by esbuild into an IIFE and
  registered as a WebDriver BiDi preload script by `installWorld`, so the world
  is constructed in the page before the app's own scripts run and is re-applied
  on every navigation. The preload also stubs `__TAURI_INTERNALS__.metadata`:
  `src/main.tsx` reads `currentWindow.label` at module top level and
  `src/lib/uiZoom.ts` reads `currentWebview`. Handlers are merged into
  `window.__wdio_mocks__`.
- Every reply is `structuredClone`d at that dispatch boundary, as the native
  IPC's deserialization does. A handler that hands back the array it keeps gives
  React the reference it already holds, and the setState that follows is dropped
  as unchanged.
- `browser.url()` and a page refresh rebuild the world from the preload.
- **Native window handles.** The app pre-builds hidden mini and reminder
  webviews, so the session's starting handle is not always the main window;
  `wdio.native.conf.ts`'s `before` switches to the handle whose
  `metadata.currentWindow.label` is `main`.

## Network safety

Every spec stays off the network. `refresh_calendars` and `mcp_apply` are mocked
in both modes, URLs in fixtures are under `example.test`, and the license and
external-link controls are addressed by testid and left unclicked.

`check_for_update` reaches the production manifest at
`https://daisy.smbr.app/updates/<os>/latest.json`, and `App.tsx` runs it 4 s
after main and every 6 h while `auto_update_check` is on. Onboarding leaves it
off: `wizard.completeFirstRun` unticks `wizard-intro-auto-update` and asserts the
box before it continues, so the sandbox profile records
`"auto_update_check": false`. The browser world answers the command from
fixtures; natively the one spec that clicks Check now mocks it
(`42-settings-c.spec.ts`).

## Known issues

`KNOWN_ISSUES` in `e2e/env.ts` maps a GitHub issue number to a one-line reason. A
spec that asserts against an open defect reads its entry and skips with that
reason logged, so the run output names what is missing. Deleting the entry
re-arms the assertion. Today it holds #117: the record title does not commit on
Enter.

## Live recording

The live-recording section of `30-record.spec.ts` runs natively where the capture
engine can open an input — Linux, through PipeWire. macOS and Windows log `no
virtual input on this OS` and skip it. The browser world answers
`start_recording` from its fixtures, so the section always runs there.
`record-mic` is the microphone `<select>`, which renders once at least one input
exists; a runner with no microphone fails that assertion.

### A Linux runner without a microphone

Two PipeWire config fragments on the runner host give the suite an input. The
first is a silent capture node the engine enumerates as a mic (`media.class`
must be the plain `Audio/Source`; the engine drops `Audio/Source/Virtual`):

```
# ~/.config/pipewire/pipewire.conf.d/10-ci-mic.conf
context.objects = [
    { factory = adapter
      args = {
          factory.name     = support.null-audio-sink
          node.name        = "e2e-mic"
          node.description = "E2E virtual mic"
          media.class      = "Audio/Source"
          audio.position   = [ MONO ]
          object.linger    = true
      }
    }
]
```

The second matters on WSL: WSLg links `$XDG_RUNTIME_DIR/pulse/native` to its
own PulseAudio, so `pactl` (which creates the `daisy-capture` sink) talks to a
server PipeWire never sees and every start fails with `DAISY-E010 ...
daisy-capture.monitor not found`. A second pipewire-pulse socket keeps PipeWire
reachable. `wdio.native.conf.ts` exports `PULSE_SERVER=unix:$XDG_RUNTIME_DIR/pipewire-pulse-native`
when that socket exists and the variable is unset; the CI job sets it explicitly.

```
# ~/.config/pipewire/pipewire-pulse.conf.d/10-ci-socket.conf
pulse.properties = {
    server.address = [ "unix:native" "unix:/run/user/1000/pipewire-pulse-native" ]
}
```

Apply with `systemctl --user restart pipewire pipewire-pulse wireplumber`, then
confirm: `pw-cli ls Node` lists `e2e-mic`, and
`PULSE_SERVER=unix:/run/user/1000/pipewire-pulse-native pactl info` reports
`PulseAudio (on PipeWire ...)`.

## Troubleshooting

**Chrome for Testing install hangs.** WebdriverIO fetches Chrome and chromedriver
on the first browser-mode run. On this mac both zips download in full and
`extract-zip` then stalls at ~432 KB with the node process at 0 % CPU (observed
twice, at 13 and 4 minutes). Extract them into the layout wdio expects and re-run:

```sh
ditto -x -k "$TMPDIR/chrome/<version>-chrome-mac-arm64.zip" \
  "$TMPDIR/chrome/mac_arm-<version>"
ditto -x -k "$TMPDIR/chromedriver/<version>-chromedriver-mac-arm64.zip" \
  "$TMPDIR/chromedriver/mac_arm-<version>"
chmod +x "$TMPDIR/chromedriver/mac_arm-<version>/chromedriver-mac-arm64/chromedriver"
```

Every run after that starts in about 3 s. The cause of the stall is undiagnosed.

**Windows: the run stops with `the main webview has no focus`, or the session
title stays in its edit field, the tag list never opens and Copy never reports.**
WebView2 dispatches focus and blur events only while the page has focus, and
Windows closes the clipboard to every process while the desktop is locked. A window shown
by a process without foreground rights opens unfocused, so `wdio.native.conf.ts`
has the app focus its own main window (`core:window:allow-set-focus` in the
ui-test capability) once that window is found, then checks `document.hasFocus()`
and stops the run with that message when it stays false, which it does for the
whole run while the runner's desktop is locked. Unlock the desktop, then keep it unlocked: Settings → Accounts
→ Sign-in options → "If you've been away, when should Windows require you to
sign in again?" → Never. WebKitGTK under xvfb reports the page focused; WKWebView
reports it unfocused yet dispatches the events and writes the clipboard, so the
check runs on Windows only.

**Windows: Copy reports, yet the title stays in its edit field and the tag
list never opens (`the rename field did not close`, `tag-combobox-option-…
wasn't found`), and every spec file's `before` hook logs `the main webview has
no focus`.** Measured 2026-09-08: the runner desktop's foreground window was a
Windows Security prompt (`PickerHost`, started 2026-09-03) raised by a field
Daisy build the moment it bound port 7845. A window held in the foreground by
another process leaves every webview the suite opens unfocused, so the only
two specs driven by a blur or a focus event fail (the title commits when
Enter calls `input.blur()`, the tag list opens on the combobox's focus) while
click- and Save-driven edits pass. Two things about that prompt:

- The firewall rule is per executable path. The `Daisy ui-test` Allow rule
  for `D:\c\release\daisy-app.exe` keeps the ui-test binary from prompting;
  any other Daisy binary on the runner host (a field install, a sandbox
  build) prompts on its own first bind, and its prompt blocks the suite just
  the same.
- `PostMessage WM_CLOSE` to the dialog does not end `PickerHost`; the process
  from 2026-09-03 was alive and back in the foreground on 2026-09-08. Answer
  the prompt (Allow, which needs an administrator), or end the process:
  `Stop-Process -Id <pid> -Force` (the same as Cancel: the Block rules for
  that binary stay).

The `Runner desktop foreground (windows)` CI step prints the foreground window
before the suite and fails when it is that prompt. To see the desktop from WSL
without a monitor:

```powershell
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
$b=[System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height
[System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($b.Location,[System.Drawing.Point]::Empty,$b.Size)
$bmp.Save("D:\desktop.png")
Get-Process PickerHost | Select-Object Id,StartTime
```

**Linux: `Embedded WebDriver server did not become ready on port 4445 within
90000ms`, the leg dies in `onPrepare`, and the backend log holds only the
`webkit compositor` line.** The app connected to a Wayland compositor that
never answered its first request: libwayland opens `$XDG_RUNTIME_DIR/wayland-0`
with `WAYLAND_DISPLAY` unset, and GDK tries Wayland before X11. On the WSL
runner that socket is WSLg's, and its compositor stops answering once its RDP
peer goes idle (`/mnt/wslg/weston.log`, `rdp_rail_idle_handler`). The config
pins `GDK_BACKEND=x11`, so a run that still shows this has the variable set to
something else in its environment. To see the hang directly:
`strace -f -e trace=connect,ppoll target/release/daisy-app` ends with a
`connect(..., "/run/user/<uid>/wayland-0")` followed by a `ppoll` that never
returns. Port 4445 being free (`ss -ltnp | grep 4445`) rules out a stale app.

**`ui-test binary missing at …`** — run `scripts/build-ui-test.sh`, or point
`DAISY_E2E_BIN` at a binary. With `CARGO_TARGET_DIR` set, both the script and the
config look under that.

**Artifacts.** `apps/frontend/e2e/.artifacts/` holds a wdio log per spec file and
a screenshot per failed test. CI uploads the directory when a job fails. An
error thrown from the config's `onComplete` hook lands only in `wdio.log` there
(the console shows nothing after the summary line) and turns the exit code to 1
with every spec passing; the sandbox removal in that hook retries and warns for
that reason.

**`gen/schemas` churn.** A ui-test build rewrites
`crates/tauri-app/gen/schemas/*.json` with the wdio permissions, and a plain
`cargo build -p tauri-app` writes them back. tauri-build regenerates them per
platform and per feature on every build; git ignores the directory.

## Findings the suite surfaced

- **A pasted summary cannot be saved.** `SummaryPane.tsx:796` renders the
  paste-your-own-summary box only while the session has no summary, and its Save
  calls `summary_save_edit`, which requires one to already exist
  (`crates/tauri-app/src/commands/summary.rs:371-373` → `AppError::Config`,
  `DAISY-E014`). Every first save of a pasted summary fails. Unfiled.
- **Issue #117** — the record title's Enter path, held by `KNOWN_ISSUES`.
