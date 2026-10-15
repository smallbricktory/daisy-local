// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { mkdirSync, mkdtempSync, rmSync, existsSync } from 'node:fs';
import { createServer } from 'node:net';
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
 *  the service's launcher, and the workers' direct-eval client alike. Windows
 *  uses its own port. */
const WEBDRIVER_PORT = process.platform === 'win32' ? 4545 : 4445;

/** Sandbox root for the run; holds the config dir and the profile. */
let runRoot = '';

/** A free TCP port, probed on loopback, for the app's sync listener
 *  (DAISY_SYNC_PORT, honored by ui-test builds). */
function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      server.close(() => (typeof address === 'object' && address ? resolve(address.port) : reject(new Error('no port bound'))));
    });
  });
}

type TauriInternals = {
  __TAURI_INTERNALS__?: {
    metadata?: { currentWindow?: { label?: string } };
    invoke?: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;
  };
};

/** On Windows, focuses the main webview and waits until the page has focus. */
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
  async onPrepare() {
    // Runs before the service launcher, which spawns the app with this env.
    // DAISY_CONFIG_DIR holds the per-machine record (EULA, consent, install
    // id), DAISY_PROFILE_DIR the user data including the key vault.
    runRoot = mkdtempSync(join(tmpdir(), 'daisy-e2e-'));
    // The profile directory name carries an @ and spaces.
    const profile = join(runRoot, 'profile @ e2e');
    const configDir = join(runRoot, 'config');
    mkdirSync(profile, { recursive: true });
    mkdirSync(configDir, { recursive: true });
    process.env.E2E_MODE = 'native';
    process.env.TAURI_WEBDRIVER_PORT = String(WEBDRIVER_PORT);
    // The Linux suite runs under xvfb-run; GDK uses the X11 backend.
    if (process.platform === 'linux') process.env.GDK_BACKEND ??= 'x11';
    // Uses the pipewire-pulse-native socket when present and PULSE_SERVER is unset
    // (docs/testing-ui.md, "Live recording").
    if (process.platform === 'linux' && !process.env.PULSE_SERVER) {
      const runtimeDir = process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid?.() ?? ''}`;
      const socket = join(runtimeDir, 'pipewire-pulse-native');
      if (existsSync(socket)) process.env.PULSE_SERVER = `unix:${socket}`;
    }
    process.env.DAISY_CONFIG_DIR = configDir;
    process.env.DAISY_PROFILE_DIR = profile;
    process.env.DAISY_SYNC_PORT = String(await freePort());
    process.env.DAISY_NEMOTRON_MODEL_DIR = models('nemotron');
    process.env.DAISY_WHISPER_MODEL_DIR = models('whisper');
    process.env.DAISY_VOICEPRINT_DIR = models('voiceprints');
    process.env.DAISY_EMBED_DIR = models('embeddings');
    process.env.DAISY_NEMOTRON3_DIAR_DIR = models('nemotron3-diar');
    console.log(`[e2e] sandbox ${runRoot}, sync port ${process.env.DAISY_SYNC_PORT}`);
  },
  /** Removes the sandbox, retrying; a sandbox that cannot be removed is reported. */
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
