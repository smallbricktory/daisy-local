const COPY_LARGE_BYTES = 5 * 1024 * 1024;

export function copyOutcomeForSize(byteLength: number, threshold = COPY_LARGE_BYTES): 'copied' | 'truncated' {
  return byteLength > threshold ? 'truncated' : 'copied';
}
