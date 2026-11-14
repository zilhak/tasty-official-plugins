import type { NotifyCursor } from '../types/index';

export const MAX_READ = 1024 * 1024;
export const MAX_BATCH = 16 * 1024;

export function retention(text: string): number {
  const value = /^retention_start=(\d+)\s*$/m.exec(text)?.[1];
  const n = Number(value ?? 0);
  if (!Number.isSafeInteger(n) || n < 0) throw new Error('invalid retention');
  return n;
}

// Offsets count bytes, including an incomplete UTF-8 tail, rather than JS characters.
export function batch(bytes: Uint8Array, start: number): { text: string; end: number } {
  let end = start;
  for (let i = start; i < Math.min(bytes.length, start + MAX_BATCH); i++) {
    if (bytes[i] === 10) end = i + 1;
  }
  // A writer can append one long line. Deliver it whole, bounded by MAX_READ.
  if (end === start) {
    const newline = bytes.indexOf(10, start);
    if (newline >= 0) end = newline + 1;
  }
  return { text: new TextDecoder().decode(bytes.slice(start, end)), end };
}

export function position(cursor: NotifyCursor, base: number, length: number): number {
  const offset = cursor.next - base;
  return cursor.missing || base < cursor.retention || offset < 0 || offset > length ? 0 : offset;
}
