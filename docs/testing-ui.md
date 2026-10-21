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

15 spec files run against one app process. CI runs both modes on the Linux and
Windows runners, in the `ui-tests` job, on every push.
The Windows runner is an interactive process on the console session, and that
desktop must stay unlocked while the suite runs (see Troubleshooting).
`43-settings-mobile-sync` starts the sync listener on 0.0.0.0. The run hands the
app a free port through `DAISY_SYNC_PORT` (honored by ui-test builds only), so
the suite passes on a host where an installed Daisy, or the other platform's
suite under WSL mirrored networking, holds the product port 7845. The listener
raises the Windows firewall prompt the first time. Allow the binary once, from
an elevated PowerShell, before the first run:

```powershell
New-NetFirewallRule -DisplayName "Daisy ui-test" -Direction Inbound -Action Allow `
  -Program "<target-dir>\release\daisy-app.exe" -Profile Any
```

The path is `$CARGO_TARGET_DIR\release\daisy-app.exe`.

The Windows leg drives its app on port 4545 and the Linux leg on 4445.

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
It compiles with the release profile at LTO off and 16 codegen units
(opt-level 3).

The run launches that binary under a sandbox minted per run:
`wdio.native.conf.ts` creates a temp root and sets `DAISY_CONFIG_DIR` (EULA
record, consent, install id) and `DAISY_PROFILE_DIR` (sessions, user data, and
the key vault) inside it, plus the model directories. `DAISY_CONFIG_DIR` is
honored only by binaries built with the `ui-test` feature (and by `daisy-cli`)
— shipping app builds ignore it, so the install record that anchors the trial
cannot be redirected by an environment variable.

- `E2E_KEEP_PROFILE=1` keeps the sandbox after the run.
- `DAISY_E2E_BIN` points the run at another binary.
- `DAISY_SYNC_PORT` is set per run to a free port for the app's sync listener.

Linux runs under its own Xvfb, whatever display the host has:
`xvfb-run -a pnpm --dir apps/frontend run e2e:native`.
`scripts/setup-dev.sh` installs `xvfb`. The config sets `GDK_BACKEND=x11` (when
unset) so the app opens on that Xvfb display. The app reaches PipeWire through
`XDG_RUNTIME_DIR`; a runner started as a systemd service exports
`/run/user/<uid>` (and the session bus) before the suite.

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

`check_for_update` reaches the production update manifest, and `App.tsx` runs it 4 s
after main and every 6 h while `auto_update_check` is on. Onboarding leaves it
off: `wizard.completeFirstRun` unticks `wizard-intro-auto-update` and asserts the
box before it continues, so the sandbox profile records
`"auto_update_check": false`. The browser world answers the command from
fixtures; natively the one spec that clicks Check now mocks it
(`42-settings-c.spec.ts`).

## Known issues

`KNOWN_ISSUES` in `e2e/env.ts` maps a GitHub issue number to a one-line reason. A
spec that asserts against an open defect reads its entry and skips with that
reason logged. Deleting the entry re-arms the assertion.

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
context.spa-libs = {
    audiotestsrc = audiotestsrc/libspa-audiotestsrc
}
context.objects = [
    { factory = adapter
      args = {
          factory.name     = audiotestsrc
          node.name        = "e2e-mic"
          node.description = "E2E virtual mic"
          media.class      = "Audio/Source"
          audio.position   = [ MONO ]
          node.param.Props = { volume = 0.0 }
          object.linger    = true
      }
    }
]
```

The node must be a real source: a `support.null-audio-sink` under
`Audio/Source` has no output ports, and WirePlumber's setup of it never
finishes, so no default sink is set and playback streams stay unlinked.

On WSL, `$XDG_RUNTIME_DIR/pulse/native` points to WSLg's PulseAudio, so `pactl`
(which creates the `daisy-capture` sink) talks to a server PipeWire does not
see. A second pipewire-pulse socket keeps PipeWire reachable.
`wdio.native.conf.ts` exports `PULSE_SERVER=unix:$XDG_RUNTIME_DIR/pipewire-pulse-native`
when that socket exists and the variable is unset.

```
# ~/.config/pipewire/pipewire-pulse.conf.d/10-ci-socket.conf
pulse.properties = {
    server.address = [ "unix:native" "unix:/run/user/1000/pipewire-pulse-native" ]
}
```

Apply with `systemctl --user restart pipewire pipewire-pulse wireplumber`, then
confirm: `pw-cli ls Node` lists `e2e-mic`,
`PULSE_SERVER=unix:/run/user/1000/pipewire-pulse-native pactl info` reports
`PulseAudio (on PipeWire ...)`, and `pw-metadata -n default` shows a
`default.audio.sink`. An empty `default` metadata means WirePlumber is stuck on
a node, and every stream stays unlinked.

## Troubleshooting

**Chrome for Testing install hangs.** WebdriverIO fetches Chrome and chromedriver
on the first browser-mode run. If `extract-zip` stalls, extract the downloaded
zips into the layout wdio expects and re-run:

```sh
ditto -x -k "$TMPDIR/chrome/<version>-chrome-mac-arm64.zip" \
  "$TMPDIR/chrome/mac_arm-<version>"
ditto -x -k "$TMPDIR/chromedriver/<version>-chromedriver-mac-arm64.zip" \
  "$TMPDIR/chromedriver/mac_arm-<version>"
chmod +x "$TMPDIR/chromedriver/mac_arm-<version>/chromedriver-mac-arm64/chromedriver"
```


**Windows: the run stops with `the main webview has no focus`, or the session
title stays in its edit field, the tag list never opens and Copy never reports.**
WebView2 dispatches focus and blur events only while the page has focus, and
Windows closes the clipboard to every process while the desktop is locked.
`wdio.native.conf.ts` has the app focus its own main window
(`core:window:allow-set-focus` in the ui-test capability), then checks
`document.hasFocus()` and stops the run with that message when it is false.
The check fails while the desktop is locked or while another process holds a
window in the foreground, such as a firewall prompt. Unlock the desktop and
set Settings → Accounts → Sign-in options → "If you've been away, when should
Windows require you to sign in again?" to Never. WebKitGTK under xvfb and
WKWebView dispatch the events, so the check runs on Windows only.

The firewall rule is per executable path: each Daisy binary prompts on its
first sync-listener bind, on any port. Answer the prompt (Allow needs an administrator) or
end the prompt's process with `Stop-Process -Id <pid> -Force`.

The `Runner desktop foreground (windows)` CI step prints the foreground window
before the suite and fails when it is a prompt or the lock screen.

**Linux: `Embedded WebDriver server did not become ready on port 4445 within
90000ms`, and the leg stops in `onPrepare`.** The app connected to a Wayland
compositor that did not answer. The config sets `GDK_BACKEND=x11`; a run that
shows this has the variable set to something else in its environment. Port 4445
being free (`ss -ltnp | grep 4445`) rules out a stale app.

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
