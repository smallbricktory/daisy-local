// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// One store over the built-in model's status as the Rust side derives it: the
// model row (installed, partial bytes, capability, warm-up) and the AI
// provider status every gate reads. It seeds from `builtin_model_status` on
// the first subscription and follows `builtin-model:changed` after that; the
// backend announces every change, a provider key saved included.

import { backendStore } from './lib/backendStore';
import { tauri, type BuiltinStatus } from './tauri';

const store = backendStore<BuiltinStatus | null>(
  'builtin-model:changed', () => tauri.builtinModelStatus(), null,
);

/** The status, or null until the first answer. */
export function useBuiltinModel(): BuiltinStatus | null {
  return store.use();
}
