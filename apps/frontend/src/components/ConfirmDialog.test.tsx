// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { render, fireEvent } from '@testing-library/react';
import { ConfirmDialog } from './ConfirmDialog';

describe('ConfirmDialog Escape', () => {
  it('cancels the only open dialog', () => {
    const onCancel = vi.fn();
    render(<ConfirmDialog title="T" body="B" confirmLabel="OK" onCancel={onCancel} onConfirm={() => {}} />);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it('cancels only the topmost of stacked dialogs', () => {
    const lower = vi.fn();
    const upper = vi.fn();
    render(<ConfirmDialog title="Lower" body="B" confirmLabel="OK" onCancel={lower} onConfirm={() => {}} />);
    const top = render(<ConfirmDialog title="Upper" body="B" confirmLabel="OK" onCancel={upper} onConfirm={() => {}} />);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(upper).toHaveBeenCalledTimes(1);
    expect(lower).not.toHaveBeenCalled();

    top.unmount();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(lower).toHaveBeenCalledTimes(1);
  });
});
