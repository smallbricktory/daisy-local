import { describe, it, expect } from 'vitest';
import { STEP_ORDER, nextStep, previousStep, type WizardAnswers } from './wizard-graph';

const BASE: WizardAnswers = {
  selectedMicId: null,
};

describe('STEP_ORDER', () => {
  it('is the 6-step first-run flow ending on the speed check', () => {
    expect(STEP_ORDER).toEqual(['welcome', 'intro', 'vault', 'ai-provider', 'microphone', 'benchmark']);
  });

  it('has no separate summary step (the benchmark step is the closing screen)', () => {
    expect(STEP_ORDER as readonly string[]).not.toContain('summary');
  });
});

describe('nextStep', () => {
  it('welcome → intro', () => {
    expect(nextStep('welcome', BASE)).toBe('intro');
  });

  it('intro → vault', () => {
    expect(nextStep('intro', BASE)).toBe('vault');
  });

  it('vault → ai-provider', () => {
    expect(nextStep('vault', BASE)).toBe('ai-provider');
  });

  it('ai-provider → microphone', () => {
    expect(nextStep('ai-provider', BASE)).toBe('microphone');
  });

  it('microphone → benchmark', () => {
    expect(nextStep('microphone', BASE)).toBe('benchmark');
  });

  it('benchmark is terminal', () => {
    expect(nextStep('benchmark', BASE)).toBe('benchmark');
  });
});

describe('previousStep — Back navigation', () => {
  it('returns null for empty history (Back hidden on welcome)', () => {
    expect(previousStep([])).toBeNull();
  });

  it('returns the last step from a single-entry history', () => {
    expect(previousStep(['welcome'])).toBe('welcome');
  });

  it('returns the most recent entry from a multi-step history', () => {
    expect(previousStep(['welcome', 'intro', 'vault'])).toBe('vault');
  });
});
