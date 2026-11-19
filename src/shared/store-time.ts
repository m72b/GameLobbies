// Pure helpers for playtime that includes a store's figure (Steam or GOG). Tested by store-time.test.mjs
// (`npm run test:web`). The backend computes the numbers; this only formats and picks them.
import type { Game, PlaytimeSource, StoreTime } from './types'
import type { StoreAddition } from './sessions'

/** "125 h", "12.5 h", "45 min", "<1 min", "0 min". */
export function formatHours(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 1) return '0 min'
  if (seconds < 60) return '<1 min'
  if (seconds < 3600) return Math.round(seconds / 60) + ' min'
  const hours = Math.round(seconds / 360) / 10
  return (Number.isInteger(hours) ? String(hours) : hours.toFixed(1)) + ' h'
}

type Timed = Pick<Game, 'id' | 'playtimeSeconds' | 'playtimeMinutes'>

/** The game's own tracked seconds (older rows only have minutes). */
export const trackedSeconds = (game: Pick<Game, 'playtimeSeconds' | 'playtimeMinutes'>): number =>
  Math.max(0, game.playtimeSeconds ?? game.playtimeMinutes * 60)

/** What counts for a game: the larger of tracked time and Steam's. Never lower than tracked. */
export function effectiveSeconds(game: Timed, times: Readonly<Record<string, StoreTime>>): number {
  return Math.max(trackedSeconds(game), times[game.id]?.effectiveSeconds ?? 0)
}

/** Whole minutes, for the minute-based displays. */
export const effectiveMinutes = (game: Timed, times: Readonly<Record<string, StoreTime>>): number =>
  Math.floor(effectiveSeconds(game, times) / 60)

/** The newer of the tracked and Steam's last-played time (ms); 0 when unknown. */
export function effectiveLastPlayed(game: Pick<Game, 'id' | 'lastPlayed'>, times: Readonly<Record<string, StoreTime>>): number {
  return Math.max(game.lastPlayed ?? 0, times[game.id]?.effectiveLastPlayed ?? 0)
}

/** The store a playtime figure comes from, as shown ("Steam", "GOG"). */
export const sourceName = (source: PlaytimeSource | undefined): string => source === 'gog' ? 'GOG' : 'Steam'

/** "Added from Steam · 125 h" (or GOG). */
export const addedLabel = (seconds: number, source: PlaytimeSource = 'steam'): string => 'Added from ' + sourceName(source) + ' · ' + formatHours(seconds)

/** The tooltip of the entry. */
export function addedTooltip(entry: Pick<StoreAddition, 'storeSeconds' | 'trackedSeconds'> & { source?: PlaytimeSource }): string {
  const name = sourceName(entry.source)
  return name + ' reports ' + formatHours(entry.storeSeconds) + '. Game Lobby recorded ' + formatHours(entry.trackedSeconds) + '. The difference is added so playtime matches ' + name + '.'
}

/** The entry for a game page's own stats; null when Steam has nothing Game Lobby did not track. */
export function additionOf(game: Pick<Game, 'id' | 'title'>, store: StoreTime | undefined): StoreAddition | null {
  if (!store || store.addedSeconds <= 0) return null
  return { gameId: game.id, gameTitle: game.title, source: store.source, seconds: store.addedSeconds, storeSeconds: store.seconds, trackedSeconds: store.trackedSeconds, syncedAt: store.syncedAt }
}

export type ActivityRow<S> = { kind: 'session'; session: S } | { kind: 'steam'; entry: StoreAddition }

/**
 * One newest-first list: the real sessions (already newest first) with each "Added from Steam"
 * entry placed by its sync date, ahead of sessions that started at or before it.
 */
export function mergeActivity<S extends { startedAt: number }>(sessions: readonly S[], entries: readonly StoreAddition[]): ActivityRow<S>[] {
  const pending = [...entries].sort((a, b) => b.syncedAt - a.syncedAt)
  const rows: ActivityRow<S>[] = []
  for (const session of sessions) {
    while (pending.length && pending[0].syncedAt >= session.startedAt) rows.push({ kind: 'steam', entry: pending.shift()! })
    rows.push({ kind: 'session', session })
  }
  for (const entry of pending) rows.push({ kind: 'steam', entry })
  return rows
}
