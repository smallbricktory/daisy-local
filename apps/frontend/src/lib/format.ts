export function formatMB(bytes: number): string {
  return (bytes / 1024 / 1024).toFixed(1);
}

export function formatETA(secs: number): string {
  if (!isFinite(secs) || secs <= 0) return '…';
  if (secs < 60) return `${Math.round(secs)}s`;
  return `${Math.floor(secs / 60)}m ${Math.round(secs % 60)}s`;
}
