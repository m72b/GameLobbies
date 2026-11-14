/**
 * Personal notes, score and controller details (`window.lobby.notes`). Everything is plain text
 * and numbers; nothing stored here is ever launched or opened.
 *
 * - `rating`: stars from 0.5 to 5 in half steps, or `null` for "not rated" (pass `null` to clear).
 * - `notes`: up to 5000 characters. Memories: text up to 1000 characters, `date` is `YYYY-MM-DD`,
 *   at most 500 per game; `screenshotId` must be one of the game's own screenshots.
 * - Controller `support`: `full`, `partial`, `none` (keyboard and mouse only) or `unknown`.
 *   `unknown` means "automatic": Steam's answer is used for Steam games. Any other level that the
 *   user picks is never overwritten by Steam (`supportSource` is `user`).
 * - `refreshInput` asks Steam (at most once a week per game) and returns the stored details.
 */
export type ControllerSupport = 'full' | 'partial' | 'none' | 'unknown'
export interface GameMemory { id: number; gameId: string; date: string; text: string; screenshotId: string | null; createdAt: number }
export interface GameNotes { rating: number | null; notes: string; updatedAt: number; memories: GameMemory[] }
export interface GameInput { support: ControllerSupport; supportSource: '' | 'steam' | 'user'; preferred: string; notes: string; updatedAt: number; steamCheckedAt: number }
export interface NotesAPI {
  get(gameId: string): Promise<GameNotes>
  setRating(gameId: string, rating: number | null): Promise<GameNotes>
  setText(gameId: string, notes: string): Promise<GameNotes>
  saveMemory(gameId: string, memory: { id?: number; date: string; text: string; screenshotId?: string | null }): Promise<GameNotes>
  deleteMemory(gameId: string, id: number): Promise<GameNotes>
  getInput(gameId: string): Promise<GameInput>
  saveInput(gameId: string, input: { support: ControllerSupport; preferred: string; notes: string }): Promise<GameInput>
  refreshInput(gameId: string): Promise<GameInput>
}
export const MAX_NOTES_CHARS = 5000
export const MAX_MEMORY_CHARS = 1000
export const MAX_PREFERRED_CHARS = 60
export const MAX_INPUT_NOTES_CHARS = 1000
