export type Store = 'Steam' | 'Epic Games' | 'GOG' | 'EA app' | 'Battle.net' | 'Xbox' | 'Ubisoft' | 'Emulator' | 'itch.io' | 'Amazon Games' | 'Custom'
export interface GameRoute { sourceId: string; store: Store; appId?: string; launchTarget: string; launchArgs?: string; installPath?: string; owned?: boolean; installed?: boolean; installStates?: Record<string, boolean>; playtimeMinutes?: number; lastPlayed?: number }
export interface Game { id: string; title: string; cover?: string; defaultCover?: string; store: Store; genre?: string; playtimeMinutes: number; playtimeSeconds?: number; lastPlayed?: number; hidden: boolean; favorite: boolean; routes: GameRoute[] }
export interface LibraryStats { total: number; imported?: number; errors?: string[]; scannedAt?: number }
export interface LibraryConnections { steamId: string; steamApiKeySet: boolean; legendaryPath: string; gogDatabasePath: string }
export interface LibraryConnectionInput { steamId: string; steamApiKey?: string; legendaryPath: string; gogDatabasePath: string }
export interface ThemeTokens { name: string; bg: string; surface: string; surface2: string; accent: string; text: string; muted: string; radius: string; gap: string; font: string; hover: string; backgroundImage?: string }
export interface Settings { scanIntervalHours: number; posterWidth: number; showBadges: boolean; animations: boolean; tracking: boolean; theme: ThemeTokens; themeUrl?: string; themeName?: string; /** Fetch missing covers from SteamGridDB after each library scan (needs a key). Default true. */ steamGridDbAutoFetch: boolean
  /** Library shows only games with an installed route. UI-only filter. Default true. */
  showOnlyInstalled: boolean
  /** SteamGridDB grid styles to request; empty = any style. Distinct values only. Default []. */
  steamGridDbStyles: SteamGridDbStyle[]
  /** Include animated grids in SteamGridDB results. Default false. */
  steamGridDbAnimated: boolean
  /** Include adult-flagged (NSFW) grids in SteamGridDB results. Default false. */
  steamGridDbNsfw: boolean
  /** Include humor-flagged grids in SteamGridDB results. Default false. */
  steamGridDbHumor: boolean
  /**
   * With `tracking` on, also detect installed games started outside Game Lobby (Steam, Epic,
   * desktop shortcuts) and track their sessions. false = only games launched from Game Lobby
   * are tracked. Default true.
   */
  detectExternalLaunches: boolean
  /**
   * Game ids that are never detected or tracked; launching one from Game Lobby only updates
   * `lastPlayed`. Distinct ids, each at most 512 characters, at most 5000. Default [].
   * Prefer `tracking.setExcluded(gameId, excluded)` to change a single game.
   */
  trackingExcludedGameIds: string[] }
/** Grid styles accepted by `Settings.steamGridDbStyles` (the backend rejects anything else). */
export const steamGridDbStyles = ['alternate', 'blurred', 'white_logo', 'material', 'no_logo'] as const
export type SteamGridDbStyle = typeof steamGridDbStyles[number]
export interface EmulatorProfile { id: string; name: string; executable: string; args: string; romFolders: string[]; extensions: string[] }
export interface PluginManifest { id: string; name: string; version: string; kind: 'library'; store: Store; manifestFolder?: string }
/** A SteamGridDB poster candidate. `url`/`thumb` are https://*.steamgriddb.com images. */
export interface SteamGridDbCover { id: number; url: string; thumb: string; width: number; height: number; style?: string; score?: number; upvotes?: number; downvotes?: number; author?: string }
/** Result of a bulk fetch: games given a cover, games with no match, and per-game error messages. */
export interface SteamGridDbFetchSummary { updated: number; notFound: number; errors: string[] }
/** Result of `steamGridDb.testKey()`. `message` is user-facing and never contains the key. */
export interface SteamGridDbKeyTest { ok: boolean; message: string }
/**
 * SteamGridDB cover art. The API key is write-only: it is stored DPAPI-encrypted by the
 * backend and only `keySet` is ever returned.
 * - status(): whether a key is stored.
 * - saveKey(key): saves a trimmed key (ASCII letters/digits, max 128); an empty string clears it.
 * - testKey(): one authenticated request to check the saved key. `{ ok: true, message:
 *   "SteamGridDB key works." }`, or `ok: false` with a friendly reason ("No SteamGridDB key is
 *   saved.", key rejected, network/rate-limit errors). Never rejects; never includes the key.
 * - search(gameId): up to 30 poster candidates in 2:3 sizes, filtered by the saved settings
 *   (steamGridDbStyles/Animated/Nsfw/Humor; epilepsy-flagged grids are always dropped). Uses the
 *   game's Steam app ID when it has a Steam route, otherwise a title search. Empty array = no match.
 * - apply(gameId, url): sets a candidate's `url` as the game's custom cover; only SteamGridDB
 *   https URLs are accepted. Returns the updated library.
 * - fetchMissing(): fills `cover` and `defaultCover` for games that have neither, using the same
 *   settings-based filters as search(). Rejects if a
 *   run is already in progress or the key is rejected. Emits `library:changed` when covers change.
 * - onFetched(cb): fires after any successful bulk run, including the automatic one after a scan.
 */
export interface SteamGridDbAPI { status(): Promise<{ keySet: boolean }>; saveKey(key: string): Promise<{ keySet: boolean }>; testKey(): Promise<SteamGridDbKeyTest>; search(gameId: string): Promise<SteamGridDbCover[]>; apply(gameId: string, url: string): Promise<Game[]>; fetchMissing(): Promise<SteamGridDbFetchSummary>; onFetched(callback: (summary: SteamGridDbFetchSummary) => void): () => void }
/** How a play session started: from Game Lobby (`launch`) or found running (`detected`). */
export type PlaySource = 'launch' | 'detected'
/**
 * A game Game Lobby is tracking right now.
 * - `state`: `launching` = launched from Game Lobby, waiting (up to 10 min) for its process;
 *   `running` = its process is running (stays `running` for ~30 s after the process exits).
 * - `startedAt`: ms epoch; the launch request time while launching, the session start while running.
 * - `sessionSeconds`: length of the current session when this was sent (0 while launching).
 *   The backend re-sends at least once a minute while something runs; count up locally from
 *   `startedAt` for a live clock.
 * - `exe`: full path of the matched process (only while running).
 */
export interface RunningGame { gameId: string; state: 'launching' | 'running'; startedAt: number; sessionSeconds: number; source: PlaySource; exe?: string }
/**
 * One stored play session. Times are ms epoch. `endedAt` is absent while the session is still
 * in progress. Sessions shorter than 10 s are never stored.
 */
export interface PlaySession { id: number; startedAt: number; endedAt?: number; seconds: number; source: PlaySource }
/**
 * Playtime summary for a game page.
 * - `totalSeconds`: the library playtime (same as `Game.playtimeSeconds`; includes playtime
 *   imported from stores, so it can exceed the sum of the sessions).
 * - `lastTwoWeeksSeconds`: session time in the last 14 days (a session crossing the boundary
 *   counts in proportion).
 * - `firstPlayed` / `lastPlayed`: ms epoch; absent when unknown.
 */
export interface PlayStats { totalSeconds: number; lastTwoWeeksSeconds: number; sessionCount: number; firstPlayed?: number; lastPlayed?: number }
/**
 * Play tracking (needs `Settings.tracking`; see also `Settings.detectExternalLaunches`).
 * - status(): games currently launching or running. Empty when tracking is off.
 * - onChanged(cb): pushed with the full list whenever a game starts/stops, changes state, and
 *   at least once a minute while something runs. Returns an unsubscribe function.
 * - sessions(gameId, limit = 50): newest first; `limit` is clamped to 1..1000.
 * - stats(gameId): totals for the game page; zeros for an unknown game.
 * - setExcluded(gameId, excluded): adds/removes the game in `Settings.trackingExcludedGameIds`
 *   and returns the saved settings (also emitted as `settings:changed`). Excluding a game that
 *   is running ends its session within a moment, keeping the time already saved.
 * A game counts as running only while one of its processes has a visible main window
 * (minimized counts); background services, tray helpers and wallpaper renderers don't.
 * `library.onChanged` also fires when tracked playtime is saved (each minute and at session end).
 */
export interface TrackingAPI { status(): Promise<RunningGame[]>; onChanged(callback: (running: RunningGame[]) => void): () => void; sessions(gameId: string, limit?: number): Promise<PlaySession[]>; stats(gameId: string): Promise<PlayStats>; setExcluded(gameId: string, excluded: boolean): Promise<Settings> }
/**
 * Game page art: candidate image URLs in priority order. Try `hero[0]`, and on an image load
 * error the next one, and so on; an empty array means no art (show a fallback).
 * Order: Steam's `library_hero.jpg` / `logo.png` (games with a Steam route), then SteamGridDB
 * heroes/logos (when a key is saved; filtered by the steamGridDbAnimated/Nsfw/Humor settings,
 * cached for 30 days). Never rejects for missing art (no key, no match, offline).
 */
export interface GameArt { hero: string[]; logo: string[] }
export interface ArtAPI { get(gameId: string): Promise<GameArt> }
export interface LobbyAPI {
  window: { minimize(): Promise<void>; toggleMaximize(): Promise<void>; close(): Promise<void>; isMaximized(): Promise<boolean> }
  library: { list(): Promise<Game[]>; scan(): Promise<LibraryStats>; launch(id: string, route?: number): Promise<void>; save(game: Partial<Game> & { id: string }): Promise<Game[]>; remove(id: string): Promise<Game[]>; merge(targetId: string, sourceId: string): Promise<Game[]>; resetPlaytime(): Promise<Game[]>; setCover(id: string, dataUrl: string | null): Promise<Game[]>; chooseCover(id: string): Promise<Game[]>; chooseInstallFolder(id: string): Promise<void>; add(title: string, target: string): Promise<Game[]>; connections(): Promise<LibraryConnections>; saveConnections(input: LibraryConnectionInput): Promise<LibraryConnections>; chooseLegendary(): Promise<string | null>; chooseGogDatabase(): Promise<string | null>; onChanged(callback: () => void): () => void }
  settings: { get(): Promise<Settings>; save(settings: Partial<Settings>): Promise<Settings>; onChanged(callback: (settings: Settings) => void): () => void; open(): Promise<void> }
  theme: { load(url: string): Promise<Settings>; reset(): Promise<Settings> }
  emulators: { list(): Promise<EmulatorProfile[]>; save(profile: EmulatorProfile): Promise<EmulatorProfile[]>; scan(): Promise<LibraryStats> }
  backup: { export(): Promise<string | null>; import(): Promise<number> }
  plugins: { list(): Promise<PluginManifest[]>; install(): Promise<PluginManifest[]> }
  steamGridDb: SteamGridDbAPI
  tracking: TrackingAPI
  art: ArtAPI
  app: { quit(): Promise<void> }
}
declare global { interface Window { lobby: LobbyAPI } }
