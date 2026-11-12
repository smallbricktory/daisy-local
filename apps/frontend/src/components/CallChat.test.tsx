// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
import { describe, it, expect, vi } from 'vitest';
import { createRef } from 'react';
import { render, screen, act } from '@testing-library/react';
import { CallChat } from './CallChat';

vi.mock('../tauri', () => ({
  tauri: {
    liveChatLoad: vi.fn().mockRejectedValue(new Error('no thread yet')),
    liveChatSendStream: vi.fn(),
  },
  errStr: (e: unknown) => String(e),
}));
vi.mock('../liveTranscript', () => ({
  getLiveTranscriptState: () => ({ finals: [] }),
}));
vi.mock('../lib/transcriptTail', () => ({
  transcriptTailSince: () => ({ text: '', endMs: 0 }),
}));
vi.mock('../lib/gatewayNotice', () => ({ showGatewayNoticeIfNeeded: () => false }));
vi.mock('./MarkdownView', () => ({ MarkdownView: () => null }));

describe('CallChat', () => {
  it('opens straight to the input, with no explanatory copy', async () => {
    await act(async () => {
      render(<CallChat sessionId="s1" live />);
    });
    // The old hint paragraph and empty-state block are gone.
    expect(screen.queryByText(/Chat reads this meeting/)).toBeNull();
    expect(screen.queryByText(/e\.g\. .what did we decide/)).toBeNull();
    // The input stays reachable for assistive tech via its label, not visible copy.
    expect(screen.getByLabelText('Ask about this meeting')).toBeTruthy();
    expect(screen.getByTestId('chat-input')).not.toHaveAttribute('placeholder');
  });

  it('forwards inputRef to the input element', async () => {
    const ref = createRef<HTMLInputElement>();
    await act(async () => {
      render(<CallChat sessionId="s1" live inputRef={ref} />);
    });
    expect(ref.current).toBe(screen.getByTestId('chat-input'));
  });
});
