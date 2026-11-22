// Pure helpers for the Statistics view. Run the tests with `npm run test:web`.
import type { StatsTopGame } from './types'

/** "66.7%", "50%", "<0.1%" for a share that rounds away, "0%" for none. Unusable input is "0%". */
export function formatPercent(value: number | undefined | null): string {
  if (value === undefined || value === null || !Number.isFinite(value) || value <= 0) return '0%'
  if (value < 0.1) return '<0.1%'
  const rounded = Math.min(100, Math.round(value * 10) / 10)
  return (Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1)) + '%'
}

/** Width of a bar in percent of its track, clamped to 0..100. A tiny positive share still shows a sliver. */
export function barWidth(part: number, whole: number): number {
  if (!Number.isFinite(part) || !Number.isFinite(whole) || part <= 0 || whole <= 0) return 0
  return Math.min(100, Math.max(1, (part / whole) * 100))
}

export type TopSortKey = 'title' | 'seconds'
export type SortDirection = 'asc' | 'desc'

export function sortTop(rows: readonly StatsTopGame[], key: TopSortKey, direction: SortDirection): StatsTopGame[] {
  const byTitle = (a: StatsTopGame, b: StatsTopGame) => a.title.localeCompare(b.title, undefined, { sensitivity: 'base' })
  const sign = direction === 'asc' ? 1 : -1
  return [...rows].sort((a, b) => key === 'title' ? sign * byTitle(a, b) : sign * (a.seconds - b.seconds) || byTitle(a, b))
}

export const defaultDirection = (key: TopSortKey): SortDirection => key === 'title' ? 'asc' : 'desc'

/** "includes Steam playtime", "includes Steam and GOG playtime"; empty when no store figure counts. */
export function storeHint(sources: readonly string[]): string {
  const list = sources.filter(Boolean)
  if (!list.length) return ''
  const names = list.length === 1 ? list[0] : list.slice(0, -1).join(', ') + ' and ' + list[list.length - 1]
  return 'includes ' + names + ' playtime'
}

/**
 * The per-game breakdown shown as a tooltip: what GameLobbies tracked, what the store reports and
 * which one counts (the larger; they are never added). `format` turns seconds into text.
 */
export function playtimeBreakdown(row: Pick<StatsTopGame, 'seconds' | 'trackedSeconds' | 'storeSeconds' | 'storeSource'>, format: (seconds: number) => string): string {
  const parts = ['GameLobbies tracked: ' + format(row.trackedSeconds)]
  if (row.storeSeconds !== undefined) {
    parts.push((row.storeSource ?? 'Store') + ' reports: ' + format(row.storeSeconds))
    parts.push('Counted: ' + format(row.seconds) + ' (the larger of the two, never added)')
  }
  return parts.join('\n')
}
