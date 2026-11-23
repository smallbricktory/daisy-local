// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect } from 'vitest';
import { STEP_ORDER, nextStep, previousStep, type WizardAnswers } from './wizard-graph';

const BASE: WizardAnswers = {
  selectedMicId: null,
};

describe('STEP_ORDER', () => {
  it('is the 5-step first-run flow ending on the plan page', () => {
    expect(STEP_ORDER).toEqual(['welcome', 'intro', 'microphone', 'names', 'plan']);
  });

  it('asks nothing about the vault, the AI provider or the model', () => {
    const steps = STEP_ORDER as readonly string[];
    expect(steps).not.toContain('vault');
    expect(steps).not.toContain('ai-provider');
    expect(steps).not.toContain('model');
  });
});

describe('nextStep', () => {
  it('welcome → intro', () => {
    expect(nextStep('welcome', BASE)).toBe('intro');
  });

  it('intro → microphone', () => {
    expect(nextStep('intro', BASE)).toBe('microphone');
  });

  it('microphone → names', () => {
    expect(nextStep('microphone', BASE)).toBe('names');
  });

  it('names → plan', () => {
    expect(nextStep('names', BASE)).toBe('plan');
  });

  it('plan is terminal', () => {
    expect(nextStep('plan', BASE)).toBe('plan');
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
    expect(previousStep(['welcome', 'intro', 'microphone', 'names'])).toBe('names');
  });
});
