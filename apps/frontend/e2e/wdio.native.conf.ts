// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { mkdirSync, mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { shared } from './wdio.shared.ts';
import { REPO_ROOT } from './env.ts';

const exe = process.platform === 'win32' ? 'daisy-app.exe' : 'daisy-app';
const targetDir = process.env.CARGO_TARGET_DIR ?? join(REPO_ROOT, 'target');
const BIN = process.env.DAISY_E2E_BIN ?? join(targetDir, 'release', exe);
if (!existsSync(BIN)) throw new Error(`ui-test binary missing at ${BIN}; run scripts/build-ui-test.sh`);

const models = (sub: string) => join(REPO_ROOT, 'models', sub);

/** The app's WebDriver port, read through TAURI_WEBDRIVER_PORT by the app,
 *  the service's launcher, and the workers' direct-eval client alike. The
 *  Windows runner shares one loopback with the WSL Linux runner on the same
 *  host (mirrored networking), and a suite on the default port there reaches
 *  whichever app bound it first. */
const WEBDRIVER_PORT = process.platform === 'win32' ? 4545 : 4445;

/** Sandbox root for the run; holds the config dir and the profile. */
let runRoot = '';

type TauriInternals = {
  __TAURI_INTERNALS__?: {
    metadata?: { currentWindow?: { label?: string } };
    invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  };
};

/** WebView2 dispatches focus and blur events only while the page has focus.
 *  A window shown by a process without foreground rights opens unfocused, so
 *  the app activates its own main window first (`core:window:allow-set-focus`
 *  in the ui-test capability); the page then stays unfocused only while the
 *  runner's desktop is locked. WebKitGTK and WKWebView dispatch the events
 *  regardless. */
async function requireWebviewFocus() {
  if (process.platform !== 'win32') return;
  await browser.execute(() => (globalThis as unknown as TauriInternals).__TAURI_INTERNALS__
    ?.invoke?.('plugin:window|set_focus', { label: 'main' }));
  await browser.waitUntil(() => browser.execute(() => document.hasFocus()), {
    timeout: 5_000,
    timeoutMsg: 'the main webview has no focus (document.hasFocus() is false), so WebView2 '
      + 'delivers no focus or blur events; unlock the runner desktop (docs/testing-ui.md, Troubleshooting)',
  });
}

export const config: WebdriverIO.Config = {
  ...shared,
  onPrepare() {
    // Runs before the service launcher, which spawns the app with this env.
    // DAISY_CONFIG_DIR holds the per-machine record (EULA, consent, install
    // id), DAISY_PROFILE_DIR the user data including the key vault.
    runRoot = mkdtempSync(join(tmpdir(), 'daisy-e2e-'));
    const profile = join(runRoot, 'profile');
    const configDir = join(runRoot, 'config');
    mkdirSync(profile, { recursive: true });
    mkdirSync(configDir, { recursive: true });
    process.env.E2E_MODE = 'native';
    process.env.TAURI_WEBDRIVER_PORT = String(WEBDRIVER_PORT);
    // The Linux suite runs under xvfb-run. GDK probes Wayland before X11, and
    // libwayland opens `$XDG_RUNTIME_DIR/wayland-0` even with WAYLAND_DISPLAY
    // unset; a compositor that holds that socket without answering (WSLg
    // after its RDP peer idles) parks the app in its first roundtrip and the
    // WebDriver server never comes up. X11 puts it on the Xvfb display.
    if (process.platform === 'linux') process.env.GDK_BACKEND ??= 'x11';
    // WSLg links $XDG_RUNTIME_DIR/pulse/native to its own PulseAudio, so the
    // capture sink the app creates through pactl never reaches PipeWire. A
    // host that configured the second pipewire-pulse socket
    // (docs/testing-ui.md, "Live recording") gets it unless PULSE_SERVER is
    // already set.
    if (process.platform === 'linux' && !process.env.PULSE_SERVER) {
      const runtimeDir = process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? ''}`;
      const socket = join(runtimeDir, 'pipewire-pulse-native');
      if (existsSync(socket)) process.env.PULSE_SERVER = `unix:${socket}`;
    }
    process.env.DAISY_CONFIG_DIR = configDir;
    process.env.DAISY_PROFILE_DIR = profile;
    process.env.DAISY_PARAKEET_MODEL_DIR = models('parakeet');
    process.env.DAISY_WHISPER_MODEL_DIR = models('whisper');
    process.env.DAISY_VAD_MODEL_DIR = models('vad');
    process.env.DAISY_VOICEPRINT_DIR = models('voiceprints');
    process.env.DAISY_EMBED_DIR = models('embeddings');
    process.env.SPEAKRS_MODELS_DIR = models('speakrs');
    console.log(`[e2e] sandbox ${runRoot}`);
  },
  /** Runs ahead of the service's shutdown hook, while the app may still be
   *  closing its log; on Windows that leaves the unlink pending and the
   *  directory removal fails until the process is gone, so the removal
   *  retries, and a sandbox that still cannot go is reported rather than
   *  counted as a failed run. */
  onComplete() {
    if (process.env.E2E_KEEP_PROFILE || !runRoot) return;
    try {
      rmSync(runRoot, { recursive: true, force: true, maxRetries: 25, retryDelay: 200 });
    } catch (e) {
      console.warn(`[e2e] sandbox left at ${runRoot}: ${(e as Error).message}`);
    }
  },
  async before() {
    // The app pre-builds hidden mini + reminder webviews, so the session's
    // starting handle is not necessarily the main window.
    for (const handle of await browser.getWindowHandles()) {
      await browser.switchToWindow(handle);
      const label = await browser.execute(
        () => (globalThis as unknown as TauriInternals).__TAURI_INTERNALS__?.metadata?.currentWindow?.label,
      );
      if (label === 'main') {
        await requireWebviewFocus();
        return;
      }
    }
    throw new Error('no webview window with label "main"');
  },
  services: [
    ['@wdio/tauri-service', {
      appBinaryPath: BIN,
      driverProvider: 'embedded',
      startTimeout: 90_000,
      statusPollTimeout: 5_000,
      captureBackendLogs: true,
      captureFrontendLogs: true,
      backendLogLevel: 'info',
      frontendLogLevel: 'warn',
    }],
  ],
  capabilities: [{ browserName: 'tauri' }],
};
