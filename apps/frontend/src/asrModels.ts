// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the transcription model rows as the Rust registry projects
// them: installed, active, bundled. It seeds from `list_asr_models` on the
// first subscription and follows `models:changed`, which the backend emits
// after an install, a delete, or a change of the active model.

import { backendStore } from './lib/backendStore';
import { tauri, type AsrModelInfo } from './tauri';

const store = backendStore<AsrModelInfo[] | null>(
  'models:changed', () => tauri.listAsrModels(), null,
);

/** The model rows, or null until the first answer. */
export function useAsrModels(): AsrModelInfo[] | null {
  return store.use();
}

/** Reads the rows again; rejects with the read's error. */
export const reloadAsrModels = store.refresh;

/** Returns the module to its pre-subscription state. Tests only. */
export const __resetAsrModelsForTests = store.reset;
