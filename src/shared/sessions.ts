// Play session history: types shared with the Rust `sessions` module, and pure helpers for the
// history view. Tested by sessions.test.mjs (`npm run test:web`).

import type { PlaySource } from './types'

/** A screenshot linked to a session; `src` is `lobby-shot://…` (show it through `coverUrl`). */
export interface SessionShot { id: string; src: string }
/**
 * One play session in the history view. Times are ms epoch; `endedAt` is absent while the
 * session is in progress. `edited`: start or end corrected by hand. `interrupted`: Game Lobby
 * closed unexpectedly (crash, shutdown); the end is the last time the game was seen running.
 */
export interface SessionEntry { id: number; gameId: string; gameTitle: string; startedAt: number; endedAt?: number; seconds: number; source: PlaySource; edited: boolean; interrupted: boolean; note?: string; screenshots: SessionShot[] }
/** Keyset position for the next page. */
export interface SessionCursor { startedAt: number; id: number }
/**
 * The read-only "Added from Steam" entry: the hours Steam has that Game Lobby did not track
 * (`seconds`, always above zero), dated at the last sync. Not a session: it has no id, cannot be
 * edited, deleted, shared or exported, and is not counted in any session total.
 */
export interface StoreAddition { gameId: string; gameTitle: string; source: 'steam'; seconds: number; storeSeconds: number; trackedSeconds: number; syncedAt: number }
/** `storeEntries` come with the first page only. */
export interface SessionPage { sessions: SessionEntry[]; next?: SessionCursor; storeEntries: StoreAddition[] }
export type ShareFormat = 'txt' | 'md'

/** Mirrors `sessions::MAX_NOTE_CHARS`. */
export const MAX_NOTE_CHARS = 2000
/** Mirrors `sessions::MAX_SESSION_SCREENSHOTS`. */
export const MAX_SESSION_SCREENSHOTS = 24
/** Mirrors `sessions::MIN_START_MS` (2000-01-01). */
export const MIN_START_MS = 946_684_800_000
/** Mirrors `sessions::MAX_SESSION_MS` (30 days). */
export const MAX_SESSION_MS = 30 * 24 * 3_600_000

const pad = (value: number) => String(value).padStart(2, '0')

/** A ms time as a `datetime-local` value ("2026-11-14T18:05") in the local time zone. */
export function toLocalInput(ms: number): string {
  const d = new Date(ms)
  return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + 'T' + pad(d.getHours()) + ':' + pad(d.getMinutes())
}

/** A `datetime-local` value (minutes or seconds precision) as ms, or null when invalid. */
export function fromLocalInput(value: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})(?::(\d{2}))?$/.exec(value.trim())
  if (!m) return null
  const [year, month, day, hour, minute, second] = m.slice(1).map(part => Number(part ?? 0))
  const d = new Date(year, month - 1, day, hour, minute, second)
  if (d.getFullYear() !== year || d.getMonth() !== month - 1 || d.getDate() !== day) return null
  return d.getTime()
}

/**
 * The first problem with corrected times, or null. The backend checks the same rules (and
 * overlap with the game's other sessions, which only it can see).
 */
export function editProblem(startedAt: number | null, endedAt: number | null, now: number): string | null {
  if (startedAt === null || endedAt === null) return 'Enter a start and an end.'
  if (startedAt < MIN_START_MS) return 'The start must be after 1 January 2000.'
  if (endedAt <= startedAt) return 'The end must be after the start.'
  if (endedAt > now) return "The end can't be in the future."
  if (endedAt - startedAt < 1000) return 'A session must be at least one second long.'
  if (endedAt - startedAt > MAX_SESSION_MS) return 'A session can be at most 30 days long.'
  return null
}

/** Session length in whole seconds for corrected times. */
export const durationSeconds = (startedAt: number, endedAt: number) => Math.max(0, Math.floor((endedAt - startedAt) / 1000))

/** Characters as the backend counts them (Unicode scalar values). */
export const noteLength = (note: string) => Array.from(note).length

/** The viewer's UTC offset at `ms`, in minutes east of UTC (what the share summary uses). */
export const utcOffsetMinutes = (ms: number) => -new Date(ms).getTimezoneOffset()

/** Backloggd's game search for a title (opened in the browser). */
export function backloggdSearchUrl(title: string): string {
  const query = title.trim().slice(0, 200)
  return 'https://backloggd.com/search/games/' + encodeURIComponent(query)
}

/** "18:05 – 20:12", "23:10 – 01:05 (+1 day)", or "18:05 – now" while running. Uses `clock` to print a time. */
export function formatRange(startedAt: number, endedAt: number | undefined, clock: (ms: number) => string): string {
  if (endedAt === undefined) return clock(startedAt) + ' – now'
  const startDay = new Date(startedAt); startDay.setHours(0, 0, 0, 0)
  const endDay = new Date(endedAt); endDay.setHours(0, 0, 0, 0)
  const days = Math.round((endDay.getTime() - startDay.getTime()) / 86_400_000)
  return clock(startedAt) + ' – ' + clock(endedAt) + (days > 0 ? ' (+' + days + (days === 1 ? ' day)' : ' days)') : '')
}

/** How a session started, for labels. */
export const sourceLabel = (source: PlaySource) => source === 'launch' ? 'Launched from Game Lobby' : 'Detected'
