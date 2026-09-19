// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { resolve } from 'node:path';
import { shared } from './wdio.shared.ts';
import { DEV_PORT, DEV_URL } from './env.ts';

const FRONTEND_DIR = resolve(import.meta.dirname, '..');

export const config: WebdriverIO.Config = {
  ...shared,
  onPrepare() {
    process.env.E2E_MODE = 'browser';
  },
  services: [
    ['@wdio/tauri-service', {
      mode: 'browser',
      devServerUrl: DEV_URL,
      devServer: {
        // The vite in node_modules, run through node — the same command under
        // every package manager and OS.
        command: `node node_modules/vite/bin/vite.js --port ${DEV_PORT} --strictPort`,
        cwd: FRONTEND_DIR,
        timeoutMs: 90_000,
      },
      clearMocks: true,
    }],
  ],
  capabilities: [{
    browserName: 'tauri',
    'goog:chromeOptions': {
      // Fake media devices: getUserMedia succeeds on hosts with no audio
      // hardware (headless CI), so the mic-level meter renders everywhere.
      args: [
        '--headless=new',
        '--window-size=1280,800',
        '--use-fake-device-for-media-stream',
        '--use-fake-ui-for-media-stream',
      ],
    },
  }],
};
