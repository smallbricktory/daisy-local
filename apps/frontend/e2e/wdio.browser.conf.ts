// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { shared } from './wdio.shared.ts';
import { DEV_PORT, DEV_URL } from './env.ts';

const FRONTEND_DIR = resolve(import.meta.dirname, '..');

/** A port in 20000-29999 that binds on both 127.0.0.1 and ::1, for
 *  chromedriver. The @wdio/utils patch makes `wdio:chromedriverOptions.port`
 *  the port wdio starts and connects to. */
async function freeDriverPort(): Promise<number> {
  const free = (port: number, host: string) =>
    new Promise<boolean>((done) => {
      const probe = createServer();
      probe.once('error', (e: NodeJS.ErrnoException) => done(e.code === 'EADDRNOTAVAIL'));
      probe.listen({ port, host }, () => probe.close(() => done(true)));
    });
  const first = 20_000 + Math.floor(Math.random() * 10_000);
  for (let i = 0; i < 10_000; i++) {
    const port = 20_000 + ((first - 20_000 + i) % 10_000);
    if ((await free(port, '127.0.0.1')) && (await free(port, '::1'))) return port;
  }
  throw new Error('no free chromedriver port in 20000-29999');
}

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
        // The vite in node_modules, run through node.
        command: `node node_modules/vite/bin/vite.js --port ${DEV_PORT} --strictPort`,
        cwd: FRONTEND_DIR,
        timeoutMs: 90_000,
      },
      clearMocks: true,
    }],
  ],
  capabilities: [{
    browserName: 'tauri',
    'wdio:chromedriverOptions': { port: await freeDriverPort() } as WebdriverIO.ChromedriverOptions,
    'goog:chromeOptions': {
      // Fake media devices: getUserMedia succeeds on hosts with no audio hardware.
      args: [
        '--headless=new',
        '--window-size=1280,800',
        '--use-fake-device-for-media-stream',
        '--use-fake-ui-for-media-stream',
      ],
    },
  }],
};
