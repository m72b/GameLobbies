// The play session history commands (Rust `sessions` module). Kept apart from `window.lobby`
// so the shared API file stays small; the renderer only sends ids, times and note text.
import { invoke } from '@tauri-apps/api/core'
import type { SessionCursor, SessionEntry, SessionPage, ShareFormat } from '../shared/sessions'

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    throw error instanceof Error ? error : new Error(String(error))
  }
}

export const sessionsApi = {
  /** Newest first; one game's sessions or (gameId null) all games'. */
  page: (gameId: string | null, cursor: SessionCursor | null, limit = 50) => call<SessionPage>('sessions_page', { gameId, cursor, limit }),
  /** Corrects a finished session's start and end. Emits `library:changed`. */
  update: (id: number, startedAt: number, endedAt: number) => call<SessionEntry>('sessions_update', { id, startedAt, endedAt }),
  /** Deletes a finished session and takes its time off the game's playtime. Emits `library:changed`. */
  remove: (id: number) => call<void>('sessions_delete', { id }),
  setJournal: (id: number, note: string, screenshotIds: string[]) => call<SessionEntry>('sessions_set_journal', { id, note, screenshotIds }),
  /** The share summary text. */
  shareText: (id: number, utcOffsetMinutes: number, format: ShareFormat) => call<string>('sessions_share_text', { id, utcOffsetMinutes, format }),
  export: (id: number, utcOffsetMinutes: number, format: ShareFormat) => call<string | null>('sessions_export', { id, utcOffsetMinutes, format })
}
