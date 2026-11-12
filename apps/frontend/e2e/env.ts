// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { resolve } from 'node:path';

export const REPO_ROOT = resolve(import.meta.dirname, '../../..');
export const E2E_PASSPHRASE = 'lantern-copper-walrus-orbit-2026';
/** Vite dev server browser mode runs against. */
export const DEV_PORT = 5199;
export const DEV_URL = process.env.E2E_DEV_URL ?? `http://localhost:${DEV_PORT}`;
export const FIXTURE_WAV = resolve(import.meta.dirname, 'fixtures/audio/clip-30s.wav');
export const ARTIFACTS = resolve(import.meta.dirname, '.artifacts');

export function isNative(): boolean {
  return process.env.E2E_MODE !== 'browser';
}

/** A session's directory inside the native run's sandboxed profile
 *  (`<DAISY_PROFILE_DIR>/sessions/<id>/`, holding manifest.json,
 *  transcript.json and the chunk wavs). Native mode only — the browser world
 *  writes nothing to disk. */
export function sessionDir(sessionId: string): string {
  const profile = process.env.DAISY_PROFILE_DIR;
  if (!profile) throw new Error('DAISY_PROFILE_DIR is unset — sessionDir() is native-mode only');
  return resolve(profile, 'sessions', sessionId);
}
