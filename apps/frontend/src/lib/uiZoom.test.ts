import { describe, it, expect } from 'vitest';
import { ZOOM_MIN, ZOOM_MAX, ZOOM_DEFAULT, clampZoom, stepZoom, zoomActionForKey } from './uiZoom';

describe('clampZoom', () => {
  it('clamps below min and above max', () => {
    expect(clampZoom(0.1)).toBe(ZOOM_MIN);
    expect(clampZoom(9)).toBe(ZOOM_MAX);
  });
  it('rounds to one decimal to avoid float drift', () => {
    expect(clampZoom(1.0000001)).toBe(1.0);
    expect(clampZoom(1.15)).toBe(1.2); // round half up at one decimal
  });
  it('treats non-finite as default', () => {
    expect(clampZoom(NaN)).toBe(ZOOM_DEFAULT);
    expect(clampZoom(undefined as unknown as number)).toBe(ZOOM_DEFAULT);
  });
});

describe('stepZoom', () => {
  it('steps in and out by one step', () => {
    expect(stepZoom(1.0, 'in')).toBe(1.1);
    expect(stepZoom(1.0, 'out')).toBe(0.9);
  });
  it('reset always returns default', () => {
    expect(stepZoom(1.7, 'reset')).toBe(ZOOM_DEFAULT);
  });
  it('does not exceed bounds', () => {
    expect(stepZoom(ZOOM_MAX, 'in')).toBe(ZOOM_MAX);
    expect(stepZoom(ZOOM_MIN, 'out')).toBe(ZOOM_MIN);
  });
});

describe('zoomActionForKey', () => {
  it('mac uses meta, ignores plain ctrl', () => {
    expect(zoomActionForKey({ key: '=', metaKey: true, ctrlKey: false, isMac: true })).toBe('in');
    expect(zoomActionForKey({ key: '=', metaKey: false, ctrlKey: true, isMac: true })).toBeNull();
  });
  it('non-mac uses ctrl', () => {
    expect(zoomActionForKey({ key: '-', metaKey: false, ctrlKey: true, isMac: false })).toBe('out');
  });
  it('maps +/=, -/_, 0', () => {
    expect(zoomActionForKey({ key: '+', metaKey: true, ctrlKey: false, isMac: true })).toBe('in');
    expect(zoomActionForKey({ key: '_', metaKey: true, ctrlKey: false, isMac: true })).toBe('out');
    expect(zoomActionForKey({ key: '0', metaKey: true, ctrlKey: false, isMac: true })).toBe('reset');
  });
  it('returns null without the platform modifier or for other keys', () => {
    expect(zoomActionForKey({ key: '=', metaKey: false, ctrlKey: false, isMac: true })).toBeNull();
    expect(zoomActionForKey({ key: 'a', metaKey: true, ctrlKey: false, isMac: true })).toBeNull();
  });
});
