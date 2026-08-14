import type { ReactNode } from 'react';

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div style={{ marginTop: 16 }}>
      <label style={{ display: 'block', fontSize: 13, color: 'var(--text-dim)', marginBottom: 4 }}>{label}</label>
      {children}
    </div>
  );
}
