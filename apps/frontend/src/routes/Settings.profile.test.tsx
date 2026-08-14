/**
 * Profile section: shows the directory actually in use (env override wins,
 * with a note naming the variable), and the Open Profile Directory button.
 */
import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock('../components/MicLevel', () => ({ MicLevel: () => null }));
vi.mock('../components/ConfirmDialog', () => ({ ConfirmDialog: () => null }));
vi.mock('../tauri', () => ({
  tauri: {
    openProfileDir: vi.fn(() => Promise.resolve('/x')),
    vaultKind: vi.fn(() => Promise.resolve('passphrase')),
    licenseStatus: vi.fn(() => Promise.resolve({ state: 'licensed' })),
  },
  errStr: (e: unknown) => String(e),
  formatBytes: () => '0 B',
  LANGUAGES: [],
}));

import { ProfileSection } from './Settings';
import { tauri as mockTauri } from '../tauri';

const base = {
  moving: false,
  onMove: () => {},
  onSwitch: () => {},
  onLock: () => {},
  onLaunchWizard: () => {},
  userDisplayName: 'Jane',
  onUserDisplayNameChange: () => {},
};

describe('ProfileSection directory display', () => {
  it('shows the saved path with no note when there is no override', () => {
    render(<ProfileSection {...base} profileDir="/home/jane/daisy" envOverride={null} />);
    expect(screen.getByText('/home/jane/daisy')).toBeInTheDocument();
    expect(screen.queryByText(/DAISY_PROFILE_DIR/)).not.toBeInTheDocument();
  });

  it('shows the override path plus a note naming the variable and the shadowed location', () => {
    render(<ProfileSection {...base} profileDir="/home/jane/daisy" envOverride="/mnt/d/Temp/profile" />);
    expect(screen.getByText('/mnt/d/Temp/profile')).toBeInTheDocument();
    expect(screen.queryByText('/home/jane/daisy')).not.toBeInTheDocument();
    const note = screen.getByText(/DAISY_PROFILE_DIR/).closest('p')!;
    expect(note.textContent).toMatch(/overrides the saved location/);
    expect(note.textContent).toContain('/home/jane/daisy');
  });

  it('Open Profile Directory reveals the folder', () => {
    render(<ProfileSection {...base} profileDir="/home/jane/daisy" envOverride={null} />);
    fireEvent.click(screen.getByRole('button', { name: 'Open Profile Directory' }));
    expect(mockTauri.openProfileDir).toHaveBeenCalledOnce();
  });

  it('Switch profile sits beside Open/Move and fires its handler', () => {
    const onSwitch = vi.fn();
    render(<ProfileSection {...base} onSwitch={onSwitch} profileDir="/home/jane/daisy" envOverride={null} />);
    const row = screen.getByRole('button', { name: 'Switch profile…' }).parentElement!;
    const labels = Array.from(row.querySelectorAll('button')).map((b) => b.textContent);
    expect(labels).toEqual(['Open Profile Directory', 'Switch profile…', 'Move profile…']);
    fireEvent.click(screen.getByRole('button', { name: 'Switch profile…' }));
    expect(onSwitch).toHaveBeenCalledOnce();
  });
});
