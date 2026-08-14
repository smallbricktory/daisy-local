import { describe, it, expect } from 'vitest';
import { copyOutcomeForSize } from './copy-state';

describe('copyOutcomeForSize', () => {
  it('returns "copied" for 0 bytes', () => {
    expect(copyOutcomeForSize(0)).toBe('copied');
  });

  it('returns "copied" at exactly the threshold (not over)', () => {
    expect(copyOutcomeForSize(5 * 1024 * 1024)).toBe('copied');
  });

  it('returns "truncated" for one byte over the threshold', () => {
    expect(copyOutcomeForSize(5 * 1024 * 1024 + 1)).toBe('truncated');
  });

  it('returns "truncated" with a custom threshold (1 MB over 500 KB)', () => {
    expect(copyOutcomeForSize(1024 * 1024, 500_000)).toBe('truncated');
  });

  it('returns "copied" with a custom threshold when under', () => {
    expect(copyOutcomeForSize(100_000, 500_000)).toBe('copied');
  });
});
