import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { AiProviderRequiredModal, AI_HELP_URL } from './AiProviderRequiredModal';

const openExternal = vi.fn();
vi.mock('../tauri', () => ({
  tauri: { openExternal: (url: string) => openExternal(url) },
}));

describe('AiProviderRequiredModal', () => {
  beforeEach(() => { openExternal.mockClear(); });

  it('renders nothing when closed', () => {
    const { container } = render(
      <AiProviderRequiredModal open={false} feature="Analysis" onClose={() => {}} onOpenProviders={() => {}} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it('shows the feature name', () => {
    render(<AiProviderRequiredModal open feature="Analysis" onClose={() => {}} onOpenProviders={() => {}} />);
    expect(screen.getByText(/Analysis needs an AI provider/i)).toBeInTheDocument();
  });

  it('Get an API key opens the external help page', () => {
    render(<AiProviderRequiredModal open feature="Q&A" onClose={() => {}} onOpenProviders={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /get an api key/i }));
    expect(openExternal).toHaveBeenCalledWith(AI_HELP_URL);
  });

  it('Open Providers navigates and closes', () => {
    const onClose = vi.fn();
    const onOpenProviders = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={onOpenProviders} />);
    fireEvent.click(screen.getByRole('button', { name: /open providers/i }));
    expect(onOpenProviders).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Close calls onClose', () => {
    const onClose = vi.fn();
    render(<AiProviderRequiredModal open feature="Q&A" onClose={onClose} onOpenProviders={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /close/i }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
