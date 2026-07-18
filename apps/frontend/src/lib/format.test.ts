import { describe, it, expect } from 'vitest';
import { formatMB, formatETA } from './format';

describe('formatMB', () => {
  it('converts bytes to MB with 1 decimal', () => {
    expect(formatMB(0)).toBe('0.0');
    expect(formatMB(1024 * 1024)).toBe('1.0');
    expect(formatMB(150 * 1024 * 1024)).toBe('150.0');
    expect(formatMB(148_500_000)).toBe('141.6');
  });
});

describe('formatETA', () => {
  it('renders an ellipsis for non-finite or non-positive', () => {
    expect(formatETA(0)).toBe('…');
    expect(formatETA(-5)).toBe('…');
    expect(formatETA(Infinity)).toBe('…');
    expect(formatETA(NaN)).toBe('…');
  });
  it('renders seconds for under 60', () => {
    expect(formatETA(5)).toBe('5s');
    expect(formatETA(59)).toBe('59s');
  });
  it('renders minutes + seconds for ≥ 60', () => {
    expect(formatETA(60)).toBe('1m 0s');
    expect(formatETA(125)).toBe('2m 5s');
    expect(formatETA(610)).toBe('10m 10s');
  });
});
