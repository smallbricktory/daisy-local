import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';

interface Props {
  defaultPath: string;
  onPick: (path: string) => void;
  disabled?: boolean;
}

export function ProfileDirPicker({ defaultPath, onPick, disabled }: Props) {
  const [useCustom, setUseCustom] = useState(false);
  const [customPath, setCustomPath] = useState<string | null>(null);

  const path = useCustom ? (customPath ?? '') : defaultPath;
  const valid = path.trim().length > 0;

  async function browse() {
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected === 'string' && selected.length > 0) {
      setCustomPath(selected);
      setUseCustom(true);
    }
  }

  return (
    <div>
      <p className="meta">Where should Daisy save your data?</p>
      <div style={{ marginTop: 12 }}>
        <label style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          <input
            type="radio"
            checked={!useCustom}
            onChange={() => setUseCustom(false)}
            disabled={disabled}
          />
          <span>Use default: <code>{defaultPath}</code></span>
        </label>
        <label style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 8 }}>
          <input
            type="radio"
            checked={useCustom}
            onChange={() => setUseCustom(true)}
            disabled={disabled}
          />
          <span>Custom location</span>
        </label>
        {useCustom && (
          <div style={{ marginTop: 8, display: 'flex', gap: 8, alignItems: 'center' }}>
            {customPath ? (
              <>
                <code style={{ flex: 1, fontSize: 13, wordBreak: 'break-all' }}>{customPath}</code>
                <button className="btn" onClick={browse} disabled={disabled} style={{ whiteSpace: 'nowrap' }}>
                  Change…
                </button>
              </>
            ) : (
              <button className="btn" onClick={browse} disabled={disabled}>
                Browse…
              </button>
            )}
          </div>
        )}
      </div>
      <div style={{ marginTop: 24 }}>
        <button
          className="btn"
          disabled={!valid || disabled}
          onClick={() => onPick(path.trim())}
        >
          Continue
        </button>
      </div>
    </div>
  );
}
