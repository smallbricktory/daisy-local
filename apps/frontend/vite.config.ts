// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { host: '127.0.0.1', port: 5173, strictPort: true },
  // VITE_UI_TEST=1 writes the e2e bundle to dist-ui-test and raises the
  // target: the e2e entry awaits its plugin import at module top level.
  // Shipping builds write dist at vite's default target.
  build: process.env.VITE_UI_TEST === '1'
    ? { outDir: 'dist-ui-test', target: 'es2022' }
    : {},
});
