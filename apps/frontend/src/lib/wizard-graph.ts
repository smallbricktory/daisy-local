// Wizard step graph, separate from Wizard.tsx; unit-testable without
// rendering the component tree.

export type WizardStep =
  | 'loading'
  | 'welcome'
  | 'intro'
  | 'vault'
  | 'finalizing-vault'
  | 'ai-provider'
  | 'microphone'
  | 'benchmark';

// First-run flow. The choices are: who you are + where data lives, how the
// vault is secured, which AI provider (if any) powers summaries, and the
// mic. The benchmark is the last step — it measures transcription speed on
// this machine, stores the live-captions verdict, and doubles as the
// closing screen. `finalizing-vault` is a transient spinner mapped to
// `vault` for progress.
export const STEP_ORDER: WizardStep[] = [
  'welcome', 'intro', 'vault', 'ai-provider', 'microphone', 'benchmark',
];

/** Human-readable label per step, used by the in-wizard "Step N / M — Label"
 *  header. Position in STEP_ORDER drives the numbering. */
export const STEP_LABELS: Record<WizardStep, string> = {
  loading:              'Loading',
  welcome:              'Welcome',
  intro:                'About you',
  vault:                'Vault',
  'finalizing-vault':   'Vault',
  'ai-provider':        'AI provider',
  microphone:           'Microphone',
  benchmark:            'Speed check',
};

export interface WizardAnswers {
  /** Persists the mic selection across Back navigation in the wizard. */
  selectedMicId: number | null;
}

export function nextStep(step: WizardStep, _ans: WizardAnswers): WizardStep {
  switch (step) {
    case 'welcome': return 'intro';
    case 'intro': return 'vault';
    case 'vault': return 'ai-provider';
    case 'ai-provider': return 'microphone';
    case 'microphone': return 'benchmark';
    case 'benchmark': return 'benchmark'; // terminal
    default: return 'benchmark';
  }
}

/** Returns the step to go back to (last entry in history), or null if there is none. */
export function previousStep(history: WizardStep[]): WizardStep | null {
  return history.length === 0 ? null : history[history.length - 1];
}
