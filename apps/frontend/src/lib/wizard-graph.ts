// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
// Wizard step graph, separate from Wizard.tsx; unit-testable without
// rendering the component tree.

export type WizardStep =
  | 'loading'
  | 'welcome'
  | 'intro'
  | 'microphone'
  | 'names'
  | 'plan';

// First-run flow. The choices are: who you are + where data lives, the mic,
// and live speaker names. The plan page shows what Daisy set up for
// everything else.
export const STEP_ORDER: WizardStep[] = [
  'welcome', 'intro', 'microphone', 'names', 'plan',
];

/** Human-readable label per step, used by the in-wizard "Step N / M — Label"
 *  header. Position in STEP_ORDER drives the numbering. */
export const STEP_LABELS: Record<WizardStep, string> = {
  loading:    'Loading',
  welcome:    'Welcome',
  intro:      'About you',
  microphone: 'Microphone',
  names:      'Speaker names',
  plan:       'Setup',
};

export interface WizardAnswers {
  /** Persists the mic selection across Back navigation in the wizard. */
  selectedMicId: number | null;
}

export function nextStep(step: WizardStep, _ans: WizardAnswers): WizardStep {
  switch (step) {
    case 'welcome': return 'intro';
    case 'intro': return 'microphone';
    case 'microphone': return 'names';
    case 'names': return 'plan';
    case 'plan': return 'plan'; // terminal
    default: return 'plan';
  }
}

/** Returns the step to go back to (last entry in history), or null if there is none. */
export function previousStep(history: WizardStep[]): WizardStep | null {
  return history.length === 0 ? null : history[history.length - 1];
}
