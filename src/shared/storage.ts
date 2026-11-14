// Pure helpers for the storage view. Run the tests with `npm run test:web`.
import type { StorageEntry } from './types'

const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']

/** 1024-based sizes: `0 B`, `512 B`, `1.5 KB`, `12 GB`. Three significant digits at most; unusable input is `Unknown`. */
export function formatBytes(bytes: number | undefined | null): string {
  if (bytes === undefined || bytes === null || !Number.isFinite(bytes) || bytes < 0) return 'Unknown'
  const whole = Math.floor(bytes)
  if (whole < 1024) return whole + ' B'
  let unit = 0
  let scaled = whole
  while (scaled >= 1024 && unit < units.length - 1) { scaled /= 1024; unit++ }
  const digits = (value: number) => value < 10 ? 2 : value < 100 ? 1 : 0
  let text = scaled.toFixed(digits(scaled))
  // Rounding up never prints 1024 of a unit.
  if (Number(text) >= 1024 && unit < units.length - 1) { scaled /= 1024; unit++; text = scaled.toFixed(digits(scaled)) }
  return String(Number(text)) + ' ' + units[unit]
}

export type StorageSort = 'size' | 'name' | 'lastPlayed'

/** Sorted copy. Size: biggest first, unknown last. Name: A-Z. Last played: most recent first, never played last. Ties by name. */
export function sortStorage(entries: readonly StorageEntry[], by: StorageSort): StorageEntry[] {
  const name = (a: StorageEntry, b: StorageEntry) => a.title.localeCompare(b.title, undefined, { sensitivity: 'base' })
  const largestFirst = (a: number | undefined, b: number | undefined, tie: number) => {
    if (a === undefined && b === undefined) return tie
    if (a === undefined) return 1
    if (b === undefined) return -1
    return b - a || tie
  }
  return [...entries].sort((a, b) => {
    if (by === 'name') return name(a, b)
    if (by === 'lastPlayed') return largestFirst(a.lastPlayed, b.lastPlayed, name(a, b))
    return largestFirst(a.bytes, b.bytes, name(a, b))
  })
}
