import { describe, it, expect } from 'vitest';
import { sideBadge } from './SummaryPane';

describe('sideBadge', () => {
  it('maps a known room speaker to Room', () => {
    expect(sideBadge('Bob', { Bob: 'room' })).toBe('Room');
  });
  it('maps a known remote speaker to Remote', () => {
    expect(sideBadge('Alice', { Alice: 'remote' })).toBe('Remote');
  });
  it('returns null for the local user', () => {
    expect(sideBadge('Me', {})).toBeNull();
  });
  it('returns null for an unmapped name', () => {
    expect(sideBadge('Stranger', { Bob: 'room' })).toBeNull();
  });
  it('returns null for empty speaker', () => {
    expect(sideBadge(null, {})).toBeNull();
  });
});
