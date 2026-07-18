import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import type { LabelModalState } from '../lib/labelModalStore';

// The modal now opens off labelModalStore (explicit "Label speakers" action),
// NOT automatically off the lifecycle phase. Control the store state directly.
let modalState: LabelModalState = { open: false, sessionId: null, title: null };
const closeLabelModal = vi.fn();
vi.mock('../lib/labelModalStore', () => ({
  useLabelModal: () => modalState,
  closeLabelModal: () => closeLabelModal(),
}));

// Leaving the labeler clears the phase + re-arms the finalize poll; spy both.
const dismissPhase = vi.fn();
const beginFinalizeWatch = vi.fn();
vi.mock('../lib/sessionLifecycle', () => ({
  dismissPhase: () => dismissPhase(),
  beginFinalizeWatch: (...a: unknown[]) => beginFinalizeWatch(...a),
}));

// Closing the labeler kicks the summary tail; spy it.
const resumeFinalize = vi.fn();
vi.mock('../lib/finalizeRunner', () => ({
  resumeFinalize: (id: string) => { resumeFinalize(id); return Promise.resolve(); },
}));

// The embedded SpeakerLabeler hits tauri + the diarize bus; stub it. The modal
// imports speakerNeedsReview from here for the footer decision — keep simple
// real-ish behavior (unlabeled → needs review).
vi.mock('./SpeakerLabeler', () => ({
  SpeakerLabeler: () => <div data-testid="labeler-stub" />,
  speakerNeedsReview: (sp: { is_user_labeled?: boolean }) => !sp?.is_user_labeled,
}));

// The modal fetches the speaker list (to switch "Finish later" → "Save") and
// subscribes to library:changed. Stub both.
const listSessionSpeakers = vi.fn().mockResolvedValue([]);
vi.mock('../tauri', () => ({
  tauri: { listSessionSpeakers: () => listSessionSpeakers() },
}));
vi.mock('../lib/libraryEvents', () => ({
  subscribeToLibrary: () => () => {},
}));

import { SpeakerLabelModal, SpeakerLabelModalView } from './SpeakerLabelModal';

beforeEach(() => {
  modalState = { open: false, sessionId: null, title: null };
  closeLabelModal.mockClear();
  dismissPhase.mockClear();
  beginFinalizeWatch.mockClear();
  resumeFinalize.mockClear();
  listSessionSpeakers.mockReset();
  listSessionSpeakers.mockResolvedValue([]);
});

describe('SpeakerLabelModalView', () => {
  it('renders nothing when closed', () => {
    const { container } = render(<SpeakerLabelModalView open={false} onClose={() => {}}>x</SpeakerLabelModalView>);
    expect(container.firstChild).toBeNull();
  });
  it('renders children when open', () => {
    render(<SpeakerLabelModalView open onClose={() => {}}><div>labeler</div></SpeakerLabelModalView>);
    expect(screen.getByText('labeler')).toBeInTheDocument();
  });
  it('scrim click closes', () => {
    const onClose = vi.fn();
    render(<SpeakerLabelModalView open onClose={onClose}><div>labeler</div></SpeakerLabelModalView>);
    fireEvent.click(screen.getByTestId('sstat-scrim'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
  it('clicking inside the modal does NOT close', () => {
    const onClose = vi.fn();
    render(<SpeakerLabelModalView open onClose={onClose}><div>labeler</div></SpeakerLabelModalView>);
    fireEvent.click(screen.getByText('labeler'));
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe('SpeakerLabelModal (connected)', () => {
  it('is closed when the store is not open', () => {
    modalState = { open: false, sessionId: null, title: null };
    render(<SpeakerLabelModal onChanged={() => {}} />);
    expect(screen.queryByText('Finish later')).toBeNull();
  });

  it('hosts the labeler + "Finish later" while speakers need review', async () => {
    modalState = { open: true, sessionId: 'sess-1', title: 'Test' };
    listSessionSpeakers.mockResolvedValue([]); // nothing reviewed
    render(<SpeakerLabelModal onChanged={() => {}} />);
    expect(await screen.findByText('Finish later')).toBeTruthy();
    expect(screen.queryByText('Save')).toBeNull();
    fireEvent.click(screen.getByText('Finish later'));
    // Leaving the labeler kicks the summary tail, clears the phase (drops its
    // toast), re-arms the progress poll, and closes the modal store.
    expect(resumeFinalize).toHaveBeenCalledWith('sess-1');
    expect(beginFinalizeWatch).toHaveBeenCalled();
    expect(dismissPhase).toHaveBeenCalled();
    expect(closeLabelModal).toHaveBeenCalled();
  });

  it('shows an amber "Save" once every speaker is labeled', async () => {
    modalState = { open: true, sessionId: 'sess-2', title: 'Test' };
    listSessionSpeakers.mockResolvedValue([
      { cluster_id: 1, display_name: 'Sam', email: null, voiceprint_id: null,
        match_confidence: null, is_user_labeled: true, sample_text: null, speech_ms: 1000 },
    ]);
    render(<SpeakerLabelModal onChanged={() => {}} />);
    expect(await screen.findByText('Save')).toBeTruthy();
    expect(screen.queryByText('Finish later')).toBeNull();
  });
});
