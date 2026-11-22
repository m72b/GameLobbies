export type Store = 'Steam' | 'Epic Games' | 'GOG' | 'EA app' | 'Battle.net' | 'Xbox' | 'Ubisoft' | 'Emulator' | 'itch.io' | 'Amazon Games' | 'Humble' | 'Custom'
/**
 * One way to launch a game.
 * Local routes (from library folders, see `RootsAPI`):
 * - `sourceId: 'rom'`: a ROM played with an emulator. Store `Emulator`; `appId` = the normalized
 *   lower-case ROM path (its identity); `launchTarget` = the ROM path; `installPath` = its folder.
 * - `sourceId: 'local'`: a PC game started directly. Store `Custom`; `appId` = the normalized game
 *   folder (or shortcut file) path; `installPath` = its folder. `launchTarget` is an `.exe`, a store
 *   launch link taken from a `.url` (never the `.url` file), or a `.lnk` the user confirmed in
 *   review (re-checked at every launch; refused if it changed).
 * Routes found by a folder scan carry the install state `root:<rootId>`; `installed` is true while
 * the last successful scan of that folder found the file.
 * - `emulatorId`: the emulator profile the USER chose for a `rom` route (scans never set it, and it
 *   counts as user data, so the game is kept when its file disappears). Absent = automatic: the
 *   best enabled, available, configured profile for `system` is picked at launch (manual profiles
 *   first, then the oldest, then catalog preference). An assigned profile that was deleted also
 *   falls back to automatic. Change it with `library.setEmulator`.
 * - `system`: a `CatalogSystem.id` (`'ps2'`, `'switch'`, `'pc'`, ...).
 */
export interface GameRoute { sourceId: string; store: Store; appId?: string; launchTarget: string; launchArgs?: string; installPath?: string; owned?: boolean; installed?: boolean; installStates?: Record<string, boolean>; playtimeMinutes?: number; lastPlayed?: number; emulatorId?: string; system?: string; /** A label only: the custom launcher whose games folder this local or ROM route was found in ("Play from" shows its name). Set by Game Lobby; saves must send it back unchanged. */ launcherId?: string; /** Added from the game page (`launchers.assign`): a `launcher:<id>` route made from another of the game's routes, or (on any other route) its `launcherId` label. `launchers.unassign` removes exactly that. Set by Game Lobby; saves must send it back unchanged. */ assigned?: boolean; /** The route's own name (library.renameRoute), shown after the store or launcher ("Xbox · Java Edition"). Display only: it never changes what runs. At most 60 characters. */ name?: string }
export interface Game { id: string; title: string; cover?: string; defaultCover?: string; store: Store; genre?: string; playtimeMinutes: number; playtimeSeconds?: number; lastPlayed?: number; hidden: boolean; favorite: boolean; routes: GameRoute[] }
/** `library.scan()` result. `local` is the library folder part, present when any folder was scanned (store scans and folder scans run side by side). */
export interface LibraryStats { total: number; imported?: number; errors?: string[]; scannedAt?: number; local?: LocalScanSummary }
/**
 * A store or launcher Game Lobby scans automatically (`library.stores()`). `id` is what
 * `Settings.disabledStores` holds; `found` is the number of installed games the last scan found
 * (absent before the first scan); `error` is set when the last scan could not read it.
 */
export interface StoreStatus { id: StoreId; name: string; enabled: boolean; found?: number; error?: string; scannedAt?: number }
export const storeIds = ['steam', 'epic', 'gog', 'ea', 'battlenet', 'ubisoft', 'amazon', 'itch', 'xbox'] as const
export type StoreId = typeof storeIds[number]
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
  trackingExcludedGameIds: string[]
  /** Check for a new release about 10 s after startup, at most once a day. Never installs without a click. Default true. */
  autoCheckUpdates: boolean
  /** Minimize Game Lobby when a game is started from it. */
  minimizeOnLaunch: boolean
  /** Start Game Lobby when Windows starts (a per-user Run entry; it starts minimized). Default false. */
  launchOnBoot: boolean
  /** Minimizing the window hides it to the notification area (tray icon) instead of the taskbar. Default false. */
  minimizeToTray: boolean
  /** The window close button hides the window to the tray; Game Lobby keeps running. Quit from the tray menu. Default false. */
  closeToTray: boolean
  /** Motion in the interface: always animate, follow Windows, short fades only, or no animation at all. Default "system". */
  motion: MotionPreference
  /** Stores that library scans skip (distinct `StoreId`s). Default []. */
  disabledStores: StoreId[]
  /** The built-in Wikipedia / Wikidata / Commons provider (no key needed). Default true. */
  wikimediaEnabled: boolean }
/** `Settings.motion`: "full" always animates (whatever Windows prefers), "system" follows the Windows setting, "reduced" keeps only short fades, "off" turns every animation off. */
export type MotionPreference = 'system' | 'full' | 'reduced' | 'off'
/** Grid styles accepted by `Settings.steamGridDbStyles` (the backend rejects anything else). */
export const steamGridDbStyles = ['alternate', 'blurred', 'white_logo', 'material', 'no_logo'] as const
export type SteamGridDbStyle = typeof steamGridDbStyles[number]
/**
 * An emulator profile. `executable` must be an absolute drive-letter path ending in `.exe`
 * (checked on save and in backups; existence is checked at launch).
 * `args` is a template split like a command line but WITHOUT any shell: whitespace separates
 * arguments, `"..."` or `'...'` group text (the quotes are removed), backslashes are literal, and
 * an unclosed quote is rejected on save. Placeholders are replaced inside the already-split
 * arguments: `{rom}` (ROM path), `{romdir}` (its folder), `{romname}` (file name without
 * extension). A path with spaces, quotes, `&` or `%` therefore stays exactly one argument;
 * `--path={rom}` becomes the single argument `--path=<path>`. Without `{rom}` the ROM path is
 * appended as the last argument. The emulator starts in its own folder.
 * - `systems`: `CatalogSystem.id`s it runs (used to pick a game's default emulator). Omitted on
 *   save = keep the stored value (`[]` for a new profile).
 * - `enabled`: omitted on save = keep (true for a new profile). Disabled profiles are never
 *   picked automatically and refuse to launch.
 * - `source`: `'discovered'` (found in an emulator folder) or `'manual'`. Read-only.
 * - `detectedAs`: `KnownEmulator.id` it was recognised as (by executable name). Read-only.
 * - `available`: the executable exists right now. Read-only (computed).
 * - `needsConfiguration`: the arguments still contain a catalog placeholder (RetroArch's
 *   `CORE_NAME`): never picked automatically and refuses to launch until edited. Read-only.
 * - `addedAt`: when the profile was first stored (ms epoch; 0 for older profiles). Read-only.
 *   Discovery adds a second executable recognised as an emulator that already has a profile
 *   DISABLED, so a stray copy never takes over.
 * A bare `{romname}` argument is refused at launch when the file name starts with `-` or `/`.
 * `romFolders` / `extensions` are legacy fields kept for older profiles; scans don't use them.
 */
export interface EmulatorProfile { id: string; name: string; executable: string; args: string; romFolders: string[]; extensions: string[]; systems?: string[]; enabled?: boolean; source?: 'discovered' | 'manual'; detectedAs?: string; available?: boolean; needsConfiguration?: boolean; addedAt?: number }
/** A system from the built-in catalog. `aliases` are folder names meaning it (matched ignoring case, spaces and punctuation: "Sony 3" = "PS3" = "PlayStation 3"). `extensions` are lower-case with a dot; empty for `pc`. */
export interface CatalogSystem { id: string; name: string; aliases: string[]; extensions: string[] }
/**
 * A known emulator. `executables`: lower-case file names recognised in emulator folders, preferred
 * build first (GUI before `nogui`). `prefixes`: lower-case name prefixes also recognised
 * (`desmume` matches `desmume_x432r_x64.exe`; helpers such as updaters, uninstallers, `-cli` or
 * launchers never match). `defaultArgs`: the template new profiles get. `needsConfiguration`: that
 * template must be edited first (RetroArch: replace CORE_NAME with a core; FinalBurn Neo: check the
 * ROM-set arguments); such profiles are never picked automatically.
 */
export interface KnownEmulator { id: string; name: string; executables: string[]; prefixes: string[]; systems: string[]; defaultArgs: string; needsConfiguration: boolean; note: string }
/** `emulators.catalog()`: static built-in data. */
export interface EmulatorCatalog { systems: CatalogSystem[]; emulators: KnownEmulator[] }
/** A path on an ignore list (executables of removed emulator profiles, or game files/folders scans skip). */
export interface IgnoredPath { path: string; addedAt: number }
export type LibraryRootKind = 'emulators' | 'games'
export interface EmulatorScanCounts { added: number; updated: number; unavailable: number }
/**
 * Game counts of a folder scan. `updated`: known games found again with a change (back after being
 * missing, or a new launch target). `removedOrUninstalled`: routes no longer found (games with user
 * data stay, marked not installed; the rest are removed). `skipped`: files that aren't games.
 * `failed`: folders that couldn't be read (or listed completely); games under them are kept.
 * `removalsHeld`: routes that looked missing but were NOT marked missing, because more than half
 * of a root's games (and more than 20) vanished at once; see `RootScanOutcome.warning`.
 */
/** `routesAdded`: routes a launcher games folder added to games already in the library. `romsMatched`: ROMs with an emulator that runs them. `needsReview`: review items left open for the scanned folders. */
export interface GameScanCounts { added: number; updated: number; removedOrUninstalled: number; skipped: number; failed: number; removalsHeld: number; routesAdded?: number; romsMatched?: number; needsReview?: number }
/**
 * One root in a scan.
 * - `ok: false`: the folder couldn't be read (e.g. offline drive); nothing in the library changed for it.
 * - `truncated`: the scan reached its entry cap. Not an error: what was found was kept and nothing
 *   was marked missing. `notice` then holds the user-facing explanation; offer a full scan
 *   (`roots.rescan(id, { full: true })`).
 * - `notice`: see `truncated`. Absent otherwise.
 * - `full`: this was a full ("scan everything") scan.
 * - `unreadable`: subfolders that couldn't be read.
 * - `warning`: the scan worked but removals were skipped (too many at once, or the folder became
 *   unreadable before they were applied); show it to the user.
 */
export interface RootScanOutcome { id: string; path: string; kind: LibraryRootKind; ok: boolean; error?: string; truncated: boolean; unreadable: number; warning?: string; notice?: string; full: boolean }
/** `LibraryRoot.lastResult`: the outcome plus that root's counts and duration (`visited`: entries listed by an emulator scan). Results stored before `full` existed lack it. */
export interface RootScanResult extends Omit<RootScanOutcome, 'full'> { full?: boolean; durationMs: number; emulators?: EmulatorScanCounts; games?: GameScanCounts; visited?: number }
/**
 * A library folder.
 * - `emulators` folders are searched for known emulator executables (quick scan: 5 folders deep,
 *   200,000 entries; full scan: 8 deep, 3,000,000). A folder holding a known emulator isn't searched
 *   further (its subfolders are that emulator's data), and data folders such as `dev_hdd0`, `cores`
 *   or `shaders` are skipped.
 * - `games` folders hold system folders (`PS2`, `Sony 3`, `Sony Playstation 2`, `Nintendo Switch`,
 *   `PC`, `PC Game`, ... at any level; the nearest one wins). The root itself may be a system folder
 *   (e.g. `M:\Game Library\Sony Playstation`): ROMs directly in it, per-game subfolders and folder
 *   games all count. Folder games: PS3 (disc layout with `PS3_DISC.SFB`/`PS3_GAME`, titled from
 *   PARAM.SFO), PS4, Wii U, Xbox 360. Under a PC folder each subfolder is one game; a numeric folder
 *   name is replaced by the executable's product name or file name. Quick scan: 8 deep, 250,000
 *   entries; full scan: 16 deep, 3,000,000.
 * Hidden/System attributes are ignored (such files and folders are scanned); links and junctions are
 * never followed. `path` is normalized (`D:\Roms`). `online`: the folder exists right now.
 */
/** `launcherId`: a custom launcher's games folder (a mixed app folder, listed with that launcher instead of under Library folders). */
export interface LibraryRoot { id: string; kind: LibraryRootKind; path: string; enabled: boolean; addedAt: number; lastScanAt?: number; lastResult?: RootScanResult; online: boolean; launcherId?: string }
/**
 * `library-scan:progress`, at most ~10 per second plus every phase change. `phase`: `emulators` or
 * `games` while a root is walked (`root` = its path, `processed` = entries seen in it so far,
 * `current` = the folder being read), then `library` (saving, duplicate check), then `done`.
 * `total` isn't known in advance and is currently always absent.
 */
export interface LocalScanProgress { phase: 'emulators' | 'games' | 'library' | 'done'; root: string; processed: number; total?: number; current?: string }
/** Result of a library folder scan (also pushed as `library-scan:finished`). `review`: open review items afterwards. `unsupportedSamples`: up to 20 skipped paths with reasons. */
export interface LocalScanSummary { roots: RootScanOutcome[]; emulators: EmulatorScanCounts; games: GameScanCounts; review: number; unsupportedSamples: string[]; durationMs: number }
export interface ReviewItemBase { id: string; status: 'open' | 'resolved'; resolution?: 'kept-separate' | 'linked' | 'ignored' | 'exe-chosen' | 'shortcut-confirmed' | 'emulator-chosen' | 'dismissed' | 'repaired' | 'route-removed' | 'merged-routes'; rootId?: string; createdAt: number; updatedAt: number }
/**
 * Something a scan won't decide on its own. `details` per `kind`:
 * - `duplicate`: different games with the same system and clean title (e.g. USA and Europe
 *   dumps, or the same game in two folders). Never merged automatically.
 * - `ambiguous-exe`: a PC game folder without one clear executable. The game isn't added until
 *   one is chosen.
 * - `unsupported`: an unknown system folder, or a PC game folder with no executable.
 * - `unreadable`: a folder that couldn't be read (games under it are kept as they were).
 * - `shortcut`: a `.lnk` with arguments or a target that isn't a plain existing `.exe`. Show
 *   `target` and `arguments` to the user; nothing is added until they confirm exactly those
 *   (`confirm-shortcut`). `folder` is the game folder it was found in (null when the shortcut
 *   lies directly in the PC folder or was picked with `addLocalGame`); `rootId` is null for picked
 *   files.
 * - `uncertain-rom`: a ROM in a launcher's games folder whose extension several systems use, with
 *   no single emulator for them. `emulators` lists the profiles that run one of `systems`; the
 *   user picks one (`choose-emulator`).
 * Rescans refresh open items instead of adding copies; resolved items stay resolved.
 */
export type ReviewItem = ReviewItemBase & (
  | { kind: 'duplicate'; details: { system: string; title: string; games: { gameId: string; title: string; paths: string[] }[] } }
  | { kind: 'ambiguous-exe'; details: { title: string; folder: string; candidates: string[]; rootId: string } }
  | { kind: 'unsupported' | 'unreadable'; details: { path: string; reason: string; rootId: string } }
  | { kind: 'missing-file'; details: { gameId: string; title: string; routeId: string; sourceId: string; system: string | null; path: string; folder: boolean; status: 'missing' | 'offline'; drive: string | null; candidates: string[] } }
  | { kind: 'health-duplicate'; details: { reasons: ('title' | 'store-id' | 'same-file')[]; games: { gameId: string; title: string; store: string; playtimeMinutes: number; paths: string[] }[] } }
  | { kind: 'duplicate-route'; details: { gameId: string; title: string; routes: { store: string; target: string; installed?: boolean }[] } }
  | { kind: 'shortcut'; details: { title: string; shortcut: string; target: string; arguments: string; folder: string | null; rootId: string | null; launcherId?: string } }
  | { kind: 'uncertain-rom'; details: { title: string; path: string; systems: { id: string; name: string }[]; emulators: { id: string; name: string; systems: string[] }[]; rootId: string } }
)
/**
 * How to resolve a review item:
 * - `keep-separate` (duplicate): they are different games; this exact set isn't reported again.
 * Linking duplicates is not a resolution: the interface opens the link review
 * (`library.linkPreview` / `library.link` with the item's id).
 * - `ignore` (any kind): `path` must be one of the item's paths (or its folder). Scans skip it from
 *   now on (undo with `review.unignore`); its game becomes not installed, or is removed when it
 *   has no user data.
 * - `choose-exe` (ambiguous-exe): `executable` must be one of `candidates`. The game is added now
 *   and later scans keep that choice. A shortcut candidate follows the shortcut rules (a `.lnk`
 *   with arguments is refused here).
 * - `confirm-shortcut` (shortcut): the user accepts the shown target and arguments; the game is
 *   added and launches the `.lnk`. Rejected if the shortcut changed since the item was made
 *   (rescan to see the new values). Use `ignore` with `details.shortcut` to skip it for good.
 * - `choose-emulator` (uncertain-rom): `emulatorId` must be one of `details.emulators`; the game is
 *   added and plays with that emulator.
 * - `dismiss` (any kind): close without changes.
 */
export type ReviewResolution = { action: 'keep-separate' } | { action: 'ignore'; path: string } | { action: 'choose-exe'; executable: string } | { action: 'confirm-shortcut' } | { action: 'choose-emulator'; emulatorId: string } | { action: 'dismiss' }
/** `library.addLocalGame()` result: the new (or already existing) game's id and the library. */
export interface AddedLocalGame { gameId: string; games: Game[] }
/**
 * Library folders. Calls reject with a user-facing message on invalid input.
 * - list(): all roots.
 * - add(kind, path): `path` must be the folder the user last chose with browse(kind) for the same
 *   kind, within 10 minutes (scans
 *   make the programs and ROMs in it launchable, so a typed path is refused). It must be an
 *   existing absolute local folder like `D:\Roms` (no network,
 *   `\\?\` or relative paths, no `.`/`..`, at most 1024 characters; not on a mapped network
 *   drive; not itself a link/junction; links in parent folders are resolved and the real location
 *   is checked and stored). Refused: the Windows folder, Roaming AppData and Game Lobby's data
 *   folder (and anything in or above them); Program Files, ProgramData, the user profile, Public
 *   and Local AppData themselves (or a folder containing them; folders inside them are fine); for
 *   `emulators` roots also Downloads, Desktop and Temp (and anything in or above them). Overlap:
 *   folders of the same kind can't repeat, contain or sit inside each other, and no folder can be
 *   both a game folder and an emulator folder; but a game folder may contain the emulator folder,
 *   or sit inside it (and the other way round). Each scan then skips the other registered folders
 *   nested in it (enabled or not), and a first-level folder of a game folder that only leads to
 *   such a folder (e.g. `Launcher` holding `Launcher\Emulators`) is skipped without a review item.
 *   Compared case-insensitively in long (non-8.3) form. Returns the updated list; does not scan.
 * - browse(kind): native folder picker; the chosen path (not added yet; add(kind, path) accepts it), or null.
 * - setEnabled(id, enabled): turned-off roots are skipped by scans; their games stay as they are.
 * - remove(id): games found only under it become not installed (removed when they have no user
 *   data); emulator profiles found in it stay. Waits for a running folder scan to finish.
 * - rescan(id?, options?): scans one root (it must be enabled) or every enabled root. A quick scan by
 *   default; `{ full: true }` uses much higher limits for very large folders (can take several
 *   minutes; still never follows links). Rejects while a folder scan is already running. Emits
 *   progress, `library:changed`, `review:changed` and `library-scan:finished`.
 * - onProgress(cb) / onFinished(cb): folder scan events, also during `library.scan()` and
 *   `emulators.scan()`. Return an unsubscribe function.
 */
/** `remove(id, mode)`: a launcher's games folder needs `mode` (`keep` its games and routes, or `remove` the routes it found and games left without any). */
export interface RootsAPI { list(): Promise<LibraryRoot[]>; add(kind: LibraryRootKind, path: string): Promise<LibraryRoot[]>; browse(kind: LibraryRootKind): Promise<string | null>; setEnabled(id: string, enabled: boolean): Promise<LibraryRoot[]>; remove(id: string, mode?: 'keep' | 'remove'): Promise<LibraryRoot[]>; rescan(id?: string, options?: { full?: boolean }): Promise<LocalScanSummary>; onProgress(callback: (progress: LocalScanProgress) => void): () => void; onFinished(callback: (summary: LocalScanSummary) => void): () => void }
/**
 * Emulator profiles.
 * - list(): every profile with `available` / `needsConfiguration` computed.
 * - save(profile): creates or updates (see `EmulatorProfile` for kept fields). New profiles are
 *   `manual` and recognised by executable name. `systems` must be catalog ids. `executable` must be
 *   the stored one or the one the user last chose with browseExecutable(), within 10 minutes (never typed).
 *   New or changed `args` must have balanced quotes, no control characters or `& | < > ^ %` or a
 *   backtick, and every file path in them must be inside the executable's folder (`cores\x.dll`;
 *   no `..`, network or other absolute paths); text starting with a placeholder is about the game
 *   file. Unchanged `args` saved by older versions are kept as they are.
 * - remove(id): deletes it; scans never re-add its executable (see ignored/unignore). Games that
 *   used it go back to automatic.
 * - scan(): searches the enabled emulator folders only (rejects while a folder scan runs).
 * - catalog(): built-in systems and known emulators.
 * - ignored() / unignore(path): executables of removed profiles.
 * - browseExecutable(): native .exe picker; the path (save() accepts it), or null when cancelled.
 */
/** What a custom launcher's games use as their target. */
export type LauncherTargetKind = 'file' | 'folder' | 'identifier'
/**
 * How Play starts a game of a launcher with a program: `openFirst` opens the launcher on its own
 * (no arguments) when it isn't running, waits for Continue, then starts the game directly from its
 * own file; `through` runs the launcher's program once with the game as an argument. Absent means
 * `through` (launchers saved before start modes).
 */
export type LauncherStartMode = 'openFirst' | 'through'
/**
 * A custom launcher (output of `launchers.list`). Its games have a route with `sourceId`
 * `launcher:<id>` and store `Custom`, or (a launcher without a program: `executable` is empty)
 * ordinary local and ROM routes found in its games folders with `launcherId` as a label.
 * `executable` and `workingDir` are shown, never sent back. `targetKinds`: what its games may use
 * (at least one); `targetKind` is the first of them.
 */
export interface CustomLauncher { id: string; name: string; iconKind: 'builtin' | 'image'; /** A built-in icon name or an icon file name in the covers folder. */ iconValue: string; /** `#rrggbb`. */ color: string; executable: string; argsTemplate: string; targetKind: LauncherTargetKind; targetKinds: LauncherTargetKind[]; /** Absent = the executable's folder. */ workingDir?: string; /** Absent = `through`. */ startMode?: LauncherStartMode; sortOrder: number; createdAt: number; /** The executable exists right now (always true without a program). */ available: boolean }
/**
 * What `launchers.save` takes: never a path. `useExecutablePick` / `useFolderPick` use the file or
 * folder just chosen with `browseExecutable` / `browseFolder` (for up to 10 minutes); otherwise the
 * stored ones are kept. A new launcher needs an executable pick, unless `noProgram` (no program,
 * arguments or working folder: its games run directly).
 */
export interface LauncherInput { id?: string; name: string; iconKind: 'builtin' | 'image'; iconValue: string; color: string; argsTemplate: string; targetKinds: LauncherTargetKind[]; workingDir: 'executable' | 'folder'; useExecutablePick?: boolean; useFolderPick?: boolean; noProgram?: boolean; /** Absent keeps the stored mode (`through` for a new launcher). Not with `noProgram`. */ startMode?: LauncherStartMode }
export interface AddedLauncherGame { gameId: string; games: Game[] }
/**
 * Custom launchers. Calls reject with a user-facing message on invalid input.
 * - browseExecutable / browseFolder: native pickers; the chosen path (to show) or null.
 * - chooseIcon: native image picker; copies the image into the app's data folder and returns its
 *   file name (send it as `iconValue` with `iconKind: 'image'`), or null.
 * - remove(id, mode): `keep` = the launcher's games stay without its route; `remove` = games that
 *   only had its route are removed as well.
 * - addGame(launcherId, title, identifier, kind): `kind` is what this game uses (needed when the
 *   launcher allows several); files and folders open a native picker (pass `identifier: null`;
 *   null result = cancelled); identifiers are typed. Without a program a game file is picked.
 * - browseGamesFolder(launcherId) / addFolder(launcherId, path): pick a games folder natively for that launcher, then add it
 *   (only the path just picked is accepted); returns every library root. Scan it with
 *   `roots.rescan(id)`; remove it with `roots.remove(id, mode)`.
 */
/** One way to pass a game to a launcher: route `routeIndex`'s file, install folder or store id. */
export interface AssignTarget { routeIndex: number; kind: LauncherTargetKind; target: string }
/**
 * A launcher as seen from a game's page (`launchers.assignments`). `listed`: the game shows under
 * it (any way). `assigned`: the game page added it (`unassign` removes it). `targets`: for a
 * launcher with a program, what it can be given (existing files and folders only; the first is the
 * default). `problem`: why it can't be assigned (already listed, or no target fits its kinds).
 */
/** What `launchers.link` did: the library, and what the new launch route passes to the launcher (null for a launcher without a program). */
export interface LinkedGame { games: Game[]; target: AssignTarget | null }
export interface AssignmentOption { launcherId: string; listed: boolean; assigned: boolean; targets: AssignTarget[]; problem?: string | null }
/**
 * What `library.launch` did. `waiting`: the game's custom launcher wasn't running, so its program
 * was started on its own; the launch waits for `library.continueLaunch(gameId, pendingId)` (once,
 * within a few minutes) or `cancelLaunch(pendingId)`.
 */
export type LaunchOutcome = { status: 'started' } | { status: 'waiting'; pendingId: string; launcherId: string; launcherName: string }
/** What launch options would run (from Rust's checks; nothing saved): the program, the whole argument list and the working folder. Empty for empty text. */
export interface ArgsPreview { program: string; argv: string[]; cwd: string }
/** `pending`: the launch still waits; `launcherRunning`: its launcher's program is running now. */
export interface PendingLaunchStatus { pending: boolean; launcherRunning: boolean }
/**
 * - assignments(gameId): every launcher as seen from that game's page.
 * - assign(gameId, launcherId, routeIndex?, kind?, options?): lists the game under the launcher.
 *   With a program, Play then runs it through the launcher with the chosen target and the
 *   optional launch options text (Rust checks it; one native dialog shows everything first;
 *   rejects when cancelled or refused, with the reason); the game's own routes stay. Returns the
 *   library.
 * - assignCheck(gameId, launcherId, routeIndex?, kind?, options): checks launch options for an
 *   assignment without saving: what would run, or rejects with the reason.
 * - link(gameId, launcherId): "Link to launcher": `assign` with nothing else. Rust picks the target
 *   from the game's own routes and adds no launch options; a launcher whose command was confirmed
 *   on this PC links without a dialog, otherwise one native dialog asks first (rejects when
 *   cancelled or refused, with the reason).
 * - unassign(gameId, launcherId): removes only what `assign` or `link` added. Returns the library.
 */
export interface LauncherAssignAPI { assignments(gameId: string): Promise<AssignmentOption[]>; assign(gameId: string, launcherId: string, routeIndex?: number, kind?: LauncherTargetKind, options?: string): Promise<Game[]>; link(gameId: string, launcherId: string): Promise<LinkedGame>; assignCheck(gameId: string, launcherId: string, routeIndex: number | undefined, kind: LauncherTargetKind | undefined, options: string): Promise<ArgsPreview>; unassign(gameId: string, launcherId: string): Promise<Game[]> }
export interface LaunchersAPI extends LauncherAssignAPI { list(): Promise<CustomLauncher[]>; save(launcher: LauncherInput): Promise<CustomLauncher[]>; remove(id: string, mode: 'keep' | 'remove'): Promise<CustomLauncher[]>; browseExecutable(): Promise<string | null>; browseFolder(): Promise<string | null>; chooseIcon(): Promise<string | null>; addGame(launcherId: string, title: string, identifier: string | null, kind?: LauncherTargetKind): Promise<AddedLauncherGame | null>; browseGamesFolder(launcherId: string): Promise<string | null>; addFolder(launcherId: string, path: string): Promise<LibraryRoot[]>; onChanged(callback: () => void): () => void }
export interface EmulatorsAPI { list(): Promise<EmulatorProfile[]>; save(profile: EmulatorProfile): Promise<EmulatorProfile[]>; remove(id: string): Promise<EmulatorProfile[]>; scan(): Promise<LocalScanSummary>; catalog(): Promise<EmulatorCatalog>; ignored(): Promise<IgnoredPath[]>; unignore(path: string): Promise<IgnoredPath[]>; browseExecutable(): Promise<string | null> }
/**
 * Scan review.
 * - list(includeResolved = false): open items, newest first (or every item).
 * - resolve(id, resolution): applies it (see `ReviewResolution`) and returns the open items.
 *   Rejects for an unknown or already resolved item, or a resolution that doesn't fit its kind.
 * - ignored() / unignore(path): game files and folders scans skip.
 * - onChanged(cb): items may have changed (after scans and resolutions).
 */
/**
 * Library health (items arrive through `review.list` as `missing-file`, `health-duplicate` and `duplicate-route`).
 * Nothing here takes a path: a repair uses a location Game Lobby found itself (`useFound(id, index)` of
 * `details.candidates`) or one the user picks in a native dialog (`locate`, same file type only). No file is
 * ever deleted. Every call returns the open review items.
 * - check(): looks for missing and moved files and for duplicates (also done quickly after each scan).
 * - removeRoute(id): removes the launch route (the caller must have confirmed it with the user).
 * - dismiss(id): "Ignore" / "Not duplicates" / "Keep both"; remembered.
 * - mergeRoutes(id): joins duplicate routes of one game. A duplicate pair is linked with the review step (`library.linkPreview` / `library.link` with the item's id).
 */
export interface HealthSummary { missing: number; offline: number; withSuggestions: number; duplicateGames: number; duplicateRoutes: number }
export interface HealthAPI { check(): Promise<HealthSummary>; useFound(id: string, index: number): Promise<ReviewItem[]>; locate(id: string): Promise<ReviewItem[]>; removeRoute(id: string, confirmed: boolean): Promise<ReviewItem[]>; dismiss(id: string): Promise<ReviewItem[]>; mergeRoutes(id: string): Promise<ReviewItem[]> }
export interface ReviewAPI { list(includeResolved?: boolean): Promise<ReviewItem[]>; resolve(id: string, resolution: ReviewResolution): Promise<ReviewItem[]>; ignored(): Promise<IgnoredPath[]>; unignore(path: string): Promise<IgnoredPath[]>; onChanged(callback: () => void): () => void }
/**
 * Linked games (`library.linkPreview` / `library.link` / `library.unlink`). A route is named by a
 * `RouteRef`: its position plus its stored `sourceId` and `launchTarget`, which must still match
 * (it only selects; nothing in it is saved). Link choices are game ids from the linked set, a
 * favourite flag and an optional custom title.
 */
export interface RouteRef { index: number; sourceId: string; launchTarget: string }
export interface LinkChoices { keep: string; titleFrom: string; customTitle?: string; coverFrom: string; heroFrom: string; notesFrom: string; igdbFrom: string; favorite: boolean }
export interface LinkPreviewGame { id: string; title: string; store: string; cover?: string; customCover: boolean; favorite: boolean; hidden: boolean; playtimeSeconds: number; sessions: number; screenshots: number; hasHero: boolean; hasNotes: boolean; igdbName?: string; igdbChosen: boolean; linked: number }
export interface LinkPreviewRoute { gameId: string; store: Store; sourceId: string; name?: string; suggested: boolean; installed?: boolean; owned?: boolean; target: string }
export interface LinkPreview { games: LinkPreviewGame[]; routes: LinkPreviewRoute[]; conflicts: { title: boolean; cover: boolean; hero: boolean; notes: boolean; igdb: boolean; favorite: boolean }; defaults: LinkChoices; playtimeSeconds: number; sessions: number }
export interface UnlinkPreview { title: string; routes: string[]; remaining: string[]; restores: boolean; sessions: number; playtimeSeconds: number }
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
 * - search(gameId, page?): one page of poster candidates in 2:3 sizes (`page` 0-based, 0 to 50;
 *   SteamGridDB returns up to 50 per page), filtered by the saved settings
 *   (steamGridDbStyles/Animated/Nsfw/Humor; epilepsy-flagged grids are always dropped). Uses the
 *   game's Steam app ID when it has a Steam route, otherwise a title search. `hasMore` says
 *   whether the next page exists; a page can be empty after filtering while `hasMore` is true.
 * - apply(gameId, url): sets a candidate's `url` as the game's custom cover; only SteamGridDB
 *   https URLs are accepted. Returns the updated library.
 * - fetchMissing(): fills `cover` and `defaultCover` for games that have neither, using the same
 *   settings-based filters as search(). Rejects if a
 *   run is already in progress or the key is rejected. Emits `library:changed` when covers change.
 * - onFetched(cb): fires after any successful bulk run, including the automatic one after a scan.
 * - searchHeroes(gameId, page?) / searchLogos(gameId, page?): one page of game page hero / logo
 *   candidates for the pickers, same paging and game matching as search(). Filtered by steamGridDbAnimated/Nsfw/Humor
 *   (grid styles don't apply; epilepsy-flagged entries and videos are always dropped). Not cached:
 *   every call asks SteamGridDB. Empty array = no match. Rejects with a friendly message when no
 *   key is saved, the key is rejected, SteamGridDB rate-limits or can't be reached, or the game
 *   doesn't exist. Pass a candidate's `url` to `art.setHero` / `art.setLogo`.
 */
/** One page of SteamGridDB picker results. */
export interface SteamGridDbCoverPage { covers: SteamGridDbCover[]; page: number; hasMore: boolean }
export interface SteamGridDbAPI { status(): Promise<{ keySet: boolean }>; saveKey(key: string): Promise<{ keySet: boolean }>; testKey(): Promise<SteamGridDbKeyTest>; search(gameId: string, page?: number): Promise<SteamGridDbCoverPage>; searchHeroes(gameId: string, page?: number): Promise<SteamGridDbCoverPage>; searchLogos(gameId: string, page?: number): Promise<SteamGridDbCoverPage>; apply(gameId: string, url: string): Promise<Game[]>; fetchMissing(): Promise<SteamGridDbFetchSummary>; onFetched(callback: (summary: SteamGridDbFetchSummary) => void): () => void }
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
export interface PlaySession { id: number; startedAt: number; endedAt?: number; seconds: number; source: PlaySource
  /** Start or end corrected by hand (see `SessionHistory`). */
  edited?: boolean
  /** Ended by a crash or shutdown; `endedAt` is the last time the game was seen running. */
  interrupted?: boolean }
/**
 * Playtime summary for a game page.
 * - `totalSeconds`: the library playtime (same as `Game.playtimeSeconds`; includes playtime
 *   imported from stores, so it can exceed the sum of the sessions).
 * - `lastTwoWeeksSeconds`: session time in the last 14 days (a session crossing the boundary
 *   counts in proportion).
 * - `firstPlayed` / `lastPlayed`: ms epoch; absent when unknown.
 */
export interface PlayStats { totalSeconds: number; lastTwoWeeksSeconds: number; sessionCount: number; firstPlayed?: number; lastPlayed?: number; /** Present when Steam reports time for the game; `totalSeconds` and `lastPlayed` already count it. */ store?: StoreTime }
/**
 * A game's playtime once a store's figure (Steam's `playtime_forever`) is counted. Computed by
 * the backend; never written to the game's tracked playtime or sessions.
 * - `seconds`: the store's figure. `trackedSeconds`: what Game Lobby recorded.
 * - `effectiveSeconds`: the larger of the two (never added). `addedSeconds`: `max(0, seconds - trackedSeconds)`.
 * - `lastPlayed`: the store's last-played time (ms); `effectiveLastPlayed`: the newer of that and the tracked one.
 * - `syncedAt`: ms of the last sync of the store's figures.
 */
/** Where a store-reported playtime figure comes from. */
export type PlaytimeSource = 'steam' | 'gog'
export interface StoreTime { source: PlaytimeSource; seconds: number; trackedSeconds: number; effectiveSeconds: number; addedSeconds: number; lastPlayed?: number; effectiveLastPlayed?: number; syncedAt: number }
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
 * Order: the user's custom hero/logo if set (see `GamePageCustom`), then Steam's
 * `library_hero_2x.jpg` (404 on older apps, so the next entry, `library_hero.jpg`, is the fallback) / `logo.png` (games with a Steam route), then SteamGridDB heroes/logos (when a
 * key is saved; filtered by the steamGridDbAnimated/Nsfw/Humor settings, cached for 30 days).
 * No URL appears twice. Never rejects for missing art (no key, no match, offline).
 * Entries may be `lobby-cover://<name>` (a file chosen with chooseHeroFile/chooseLogoFile):
 * pass every entry through `coverUrl()` from lobby-api.ts before using it as an image source.
 */
export interface GameArt { hero: string[]; logo: string[]; /** Licence and credit of each Wikimedia Commons entry in `hero`/`logo`, matched by `display` (the listed value). */ credits?: WikiImage[] }
/** Logo placements accepted by `art.setLogoLayout` (the backend rejects anything else). */
export const logoPositions = ['bottom-left', 'top-center', 'center', 'bottom-center'] as const
export type LogoPosition = typeof logoPositions[number]
/** Inclusive range accepted for `GamePageCustom.logoScale`. */
export const logoScaleRange = { min: 0.5, max: 2 } as const
/**
 * A game's page customization. `hero` / `logo` are absent when the automatic art is used;
 * otherwise a SteamGridDB https URL or `lobby-cover://<name>` (use `coverUrl()` to display).
 * `logoScale` multiplies the logo's normal size. Defaults: `defaultGamePageCustom` in defaults.ts
 * (`bottom-left`, `1`).
 */
export interface GamePageCustom { hero?: string; logo?: string; logoPosition: LogoPosition; logoScale: number }
/** `art.get()` result: candidates (a custom choice already first) plus the customization. */
export interface GamePageArt extends GameArt { custom: GamePageCustom }
/** Payload of `art.onChanged`. */
export interface ArtChangedEvent { gameId: string }
/**
 * Game page art and its customization. Every mutation resolves with the game's updated
 * `GamePageCustom` and emits `art:changed` (`onChanged`) so open game pages can call get() again.
 * Mutations reject with a user-facing message for an unknown game or invalid input.
 * - get(gameId): candidates + customization; an unknown game gets empty lists and defaults.
 * - setHero(gameId, url) / setLogo(gameId, url): `url` is a SteamGridDB https image URL (e.g. a
 *   `SteamGridDbCover.url` from searchHeroes/searchLogos; no other hosts, no http, no ports or
 *   credentials, no .webm) or a `lobby-cover://<name>` value whose file exists in the covers
 *   folder (e.g. a game's current custom cover or an earlier custom hero/logo). `null` resets to
 *   the automatic art.
 * - chooseHeroFile(gameId) / chooseLogoFile(gameId): native file dialog for PNG, JPEG, WebP or
 *   AVIF (at most 20 MB; the content must be a real image of one of those types). The file is
 *   copied into the covers folder and stored as `lobby-cover://...`. Cancelling resolves with the
 *   current value unchanged (no event). Transparent PNG/WebP suit logos best.
 * - setLogoLayout(gameId, position, scale): `position` from `logoPositions`, `scale` a finite
 *   number within `logoScaleRange` (0.5 to 2).
 * - onChanged(cb): fires with `{ gameId }` after any change above, and for each restored entry
 *   after a backup import. Returns an unsubscribe function.
 * Replaced custom files stay on disk; nothing is deleted.
 */
export interface ArtAPI {
  get(gameId: string): Promise<GamePageArt>
  setHero(gameId: string, url: string | null): Promise<GamePageCustom>
  setLogo(gameId: string, url: string | null): Promise<GamePageCustom>
  chooseHeroFile(gameId: string): Promise<GamePageCustom>
  chooseLogoFile(gameId: string): Promise<GamePageCustom>
  setLogoLayout(gameId: string, position: LogoPosition, scale: number): Promise<GamePageCustom>
  onChanged(callback: (event: ArtChangedEvent) => void): () => void
}
/**
 * `updates.check()` result. `version`, `date` (ms since the epoch) and `notes` (plain text, at most
 * about 4 KB; render as text, never as HTML) are present when `available`.
 */
export interface UpdateInfo { available: boolean; currentVersion: string; version?: string; date?: number; notes?: string }
/** `updates.onProgress`: bytes downloaded so far; `total` when the server sent a size. Throttled. */
export interface UpdateProgress { downloaded: number; total?: number }
/** Settings pages `settings.open(section)` can show. */
export type SettingsSection = 'updates' | 'cover-art' | 'metadata' | 'api-keys' | 'accounts'
/**
 * Signed updates (all work happens in the backend; one check or install at a time, a concurrent
 * call rejects).
 * - check(): asks the release server; rejects with a user-facing message (offline, no release yet,
 *   timeout, ...). Only newer versions count as available.
 * - autoCheck(): the startup check. Resolves null without asking the server when `autoCheckUpdates`
 *   is off or the last check was under 24 h ago; never rejects. Main window only.
 * - last(): the update found by the last check in this run, or null (no request).
 * - install(): downloads the update found by the last check, verifies its signature, ends play
 *   sessions, backs up the library and starts the installer; Game Lobby then closes and restarts.
 *   Rejects (nothing installed) when the signature does not match or the download fails.
 * - onReady: the installer is about to start (show "Restarting…").
 */
export interface UpdatesAPI {
  check(): Promise<UpdateInfo>
  autoCheck(): Promise<UpdateInfo | null>
  last(): Promise<UpdateInfo | null>
  install(): Promise<void>
  onProgress(callback: (progress: UpdateProgress) => void): () => void
  onReady(callback: () => void): () => void
}
/**
 * IGDB metadata (`window.lobby.igdb`). Credentials are a Twitch application's Client ID and
 * Client Secret; they are stored encrypted in the backend and never returned (status() only
 * reports whether each is set). All text is plain text (sanitized and length-capped in Rust):
 * render it as React text, never as HTML.
 * - `criticRating` is IGDB's aggregated_rating(_count), `userRating` its rating(_count); scores
 *   are rounded 0–100 and each is omitted when IGDB has none.
 * - get(gameId) answers from the local cache at once. When the cache is missing or older than 30
 *   days and credentials are saved, a background fetch starts (`pending: true`) and
 *   `onMetadata` fires `{ gameId, changed }` when it ends; call get() again when `changed`.
 * - search(gameId, query?) searches IGDB for the game's cleaned title, or for `query` (plain
 *   text). choose(gameId, igdbId) stores the user's pick, which always wins over automatic
 *   matches; resetMatch(gameId) forgets it and matches automatically again.
 * - fetchAll() starts a rate-limited background fetch for every game whose cache is missing or
 *   stale and returns at once; progress arrives through onProgress (and progress()).
 */
export interface IgdbRating { score: number; count?: number }
/** Seconds from IGDB's game_time_to_beats: hastily = main story, normally = main + extras, completely = completionist. */
export interface IgdbTimeToBeatSeconds { hastily?: number; normally?: number; completely?: number; count?: number }
export interface IgdbMetadata { igdbId: number; name: string; summary?: string; storyline?: string; genres: string[]; developers: string[]; publishers: string[]; criticRating?: IgdbRating; userRating?: IgdbRating; releaseDate?: number; url?: string; screenshots?: string[]; artworks?: string[]; timeToBeat?: IgdbTimeToBeatSeconds; shape?: number }
/** A screenshot or artwork: `thumb`/`full` are a cached `lobby-cover://` value once downloaded, otherwise an https URL on images.igdb.com. */
export interface IgdbImage { kind: 'screenshot' | 'artwork'; thumb: string; full: string }
/** Time to beat formatted for display ("42½ h"); absent values are left out. */
export interface IgdbTimeToBeat { mainStory?: string; mainExtras?: string; completionist?: string; count?: number }
export interface IgdbMetadataView { configured: boolean; pending: boolean; checked: boolean; metadata?: IgdbMetadata; source?: 'auto' | 'user'; fetchedAt?: number; images?: IgdbImage[]; timeToBeat?: IgdbTimeToBeat }
export interface IgdbSearchResult { igdbId: number; name: string; releaseDate?: number; platforms: string[]; mainGame: boolean }
export interface IgdbCredentialStatus { clientIdSet: boolean; clientSecretSet: boolean }
export interface IgdbTestResult { ok: boolean; message: string }
/** Outcome of the last Steam owned-games request; `message` never contains the key or the SteamID. */
export interface SteamOwnedImportStatus { ok: boolean; message: string; checkedAt: number; needsNewCredentials: boolean }
/** The optional Steam Web API owned-games import. The key and SteamID64 are write-only: only whether each is saved comes back. */
export interface SteamOwnedStatus { keySet: boolean; steamIdSet: boolean; last: SteamOwnedImportStatus | null }
export interface SteamOwnedAPI {
  status(): Promise<SteamOwnedStatus>
  /** Validates and saves the non-empty fields (an empty field keeps the stored value), then imports owned games at once when both are saved. */
  save(key: string, steamId: string): Promise<SteamOwnedStatus>
  /** Clears the key and the SteamID64 and removes the not-installed games the import added (games with your data stay). */
  remove(): Promise<SteamOwnedStatus>
}
/** A store with an account sign-in (now or later). Only `steam` signs in in this version. */
export type AccountStore = 'steam' | 'gog' | 'battlenet' | 'xbox' | 'epic'
/** `account`: owned games and store playtime come from the account; `installed`: only games installed on this PC. */
export type AccountMode = 'account' | 'installed'
/** One store account as the backend reports it. Never a credential: only flags and fixed-text messages. */
export interface AccountStatus {
  store: AccountStore; name: string
  /** This version can sign in to the store. */
  available: boolean
  mode: AccountMode; signedIn: boolean
  /** Steam: the Web API key is saved (the owned-games list needs it). */
  keySet: boolean
  /** A sign-in for this store is running (Steam: waiting for the browser). */
  signingIn: boolean
  /** `needsSignIn`: only signing in again can fix it. */
  last: { ok: boolean; message: string; checkedAt: number; needsSignIn?: boolean } | null
  /** The last sign-in or account action's outcome. */
  notice: { ok: boolean; message: string; at: number } | null
}
/**
 * Store accounts (`window.lobby.accounts`). The renderer names a store and a mode only; the
 * sign-in, its URLs and every credential stay in Rust. `signIn` starts it (Steam: in the
 * default web browser) and returns at once; the outcome arrives through `onChanged`.
 */
export interface AccountsAPI {
  status(): Promise<AccountStatus[]>
  signIn(store: AccountStore): Promise<AccountStatus[]>
  cancelSignIn(): Promise<AccountStatus[]>
  /** Forgets the account (Steam: the SteamID and the Web API key) and removes the not-installed games it added. */
  signOut(store: AccountStore): Promise<AccountStatus[]>
  setMode(store: AccountStore, mode: AccountMode): Promise<AccountStatus[]>
  /** Syncs now, ignoring the scan throttle. */
  sync(store: AccountStore): Promise<AccountStatus[]>
  /** GOG: the address GOG showed after signing in in the browser (or its code). Checked in Rust; rejects with a fixed message. */
  submitCode(store: AccountStore, code: string): Promise<AccountStatus[]>
  onChanged(callback: (statuses: AccountStatus[]) => void): () => void
}
/** A store the first-time setup asks about. */
export type SetupStore = 'steam' | 'epic' | 'gog' | 'xbox' | 'battlenet' | 'ea' | 'ubisoft' | 'itch'
export type SetupChoice = 'signin' | 'installed' | 'skip'
export interface SetupState { version: number; completedAt?: number; skippedAt?: number; choices: Partial<Record<SetupStore, SetupChoice>> }
export interface SetupStoreInfo { id: SetupStore; name: string; signIn: boolean; note: string }
/** `show`: open the setup now (fresh install); `banner`: offer it in the library (existing library). */
export interface SetupInfo { state: SetupState | null; show: boolean; banner: boolean; stores: SetupStoreInfo[] }
/** Only these fields; `completed` / `skipped` may only be true (Rust sets the time). */
export interface SetupPatch { choices?: Partial<Record<SetupStore, SetupChoice>>; completed?: true; skipped?: true }
export interface SetupAPI { get(): Promise<SetupInfo>; save(patch: SetupPatch): Promise<SetupInfo> }
/**
 * The built-in Wikidata / Wikipedia / Wikimedia Commons provider (`window.lobby.wikimedia`). No
 * key; off when `Settings.wikimediaEnabled` is false (then `enabled` is false and only the credit
 * of a Commons cover already in use is reported).
 * - get(gameId): the cached view at once; a background lookup follows when needed (`onMetadata`).
 * - search(gameId, query?): game items for the title or `query`; choose(gameId, qid) keeps the
 *   user's pick (it wins over automatic matches); resetMatch forgets it.
 * - useLogo / useCover(gameId, src): `src` must be one of this game's offered `logos` / `covers`.
 * Text is plain; render it as text. Release dates are `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
 */
export interface WikiImage { file: string; src: string; display: string; kind: 'logo' | 'cover'; width: number; height: number; originalWidth: number; originalHeight: number; licence: string; licenceUrl?: string; artist?: string; credit?: string; sourcePage: string; attributionRequired: boolean; restrictions?: string; lowTrust: boolean }
export interface WikiArticle { title: string; url: string; text: string }
export interface WikiMetadata { qid: string; label: string; description?: string; release?: string; platforms: string[]; developers: string[]; publishers: string[]; genres: string[]; steam: string[]; gog: string[]; epic: string[]; igdb: string[]; mobygames: string[]; article?: WikiArticle }
export interface WikiCandidate { qid: string; label: string; description?: string; year?: number }
export interface WikiView { enabled: boolean; pending: boolean; checked: boolean; status?: 'matched' | 'none' | 'ambiguous'; metadata?: WikiMetadata; source?: 'auto' | 'user'; matchedBy?: string; itemUrl?: string; fetchedAt?: number; candidates: WikiCandidate[]; logos: WikiImage[]; covers: WikiImage[]; coverCredit?: WikiImage; textLicence?: { name: string; url: string } }
export interface WikimediaAPI {
  get(gameId: string): Promise<WikiView>
  search(gameId: string, query?: string): Promise<WikiCandidate[]>
  choose(gameId: string, qid: string): Promise<WikiView>
  resetMatch(gameId: string): Promise<WikiView>
  useLogo(gameId: string, src: string): Promise<GamePageCustom>
  useCover(gameId: string, src: string): Promise<Game[]>
  onMetadata(callback: (event: IgdbMetadataEvent) => void): () => void
}
export interface IgdbBulkProgress { running: boolean; done: number; total: number; updated: number; notFound: number; failed: number; message?: string }
export interface IgdbMetadataEvent { gameId: string; changed: boolean }
export interface IgdbAPI {
  status(): Promise<IgdbCredentialStatus>
  /** Saves the non-empty fields; an empty field keeps the stored value. */
  saveCredentials(clientId: string, clientSecret: string): Promise<IgdbCredentialStatus>
  clearCredentials(): Promise<IgdbCredentialStatus>
  test(): Promise<IgdbTestResult>
  get(gameId: string): Promise<IgdbMetadataView>
  search(gameId: string, query?: string): Promise<IgdbSearchResult[]>
  choose(gameId: string, igdbId: number): Promise<IgdbMetadataView>
  resetMatch(gameId: string): Promise<IgdbMetadataView>
  fetchAll(): Promise<IgdbBulkProgress>
  cancelFetchAll(): Promise<void>
  progress(): Promise<IgdbBulkProgress>
  onMetadata(callback: (event: IgdbMetadataEvent) => void): () => void
  onProgress(callback: (progress: IgdbBulkProgress) => void): () => void
}
/**
 * The user's own screenshots of a game (`window.lobby.screenshots`). Files are copied into Game
 * Lobby's data folder; the originals are never changed. `src` is `lobby-shot://<dir>/<file>`
 * (show it through `coverUrl`). Lists are newest first.
 * - add(gameId): opens the native file picker (PNG, JPEG, WebP, GIF, BMP up to 40 MB each; BMP
 *   is stored as PNG) and copies the chosen files. The same picture is stored once per game.
 * - importSteam(gameId): copies the game's screenshots from the local Steam client (needs a
 *   Steam route; read only; newest 200). Rejects when none are found.
 * - remove(id): deletes Game Lobby's copy only. setAsHero(id): uses a copy as the page hero.
 * - onChanged fires `{ gameId }` after adds and removals.
 */
/** `missing`: the stored file is gone (the row is kept); adding the same picture again restores it. */
export interface Screenshot { id: string; gameId: string; src: string; width: number; height: number; addedAt: number; source: 'user' | 'steam'; missing: boolean }
/** `restored`: pictures already listed whose missing file was written back. */
export interface AddScreenshotsResult { screenshots: Screenshot[]; added: number; duplicates: number; restored: number; errors: string[] }
export interface ScreenshotsAPI {
  list(gameId: string): Promise<Screenshot[]>
  add(gameId: string): Promise<AddScreenshotsResult>
  importSteam(gameId: string): Promise<AddScreenshotsResult>
  remove(id: string): Promise<Screenshot[]>
  setAsHero(id: string): Promise<GamePageCustom>
  onChanged(callback: (event: { gameId: string }) => void): () => void
}
import type { NotesAPI } from './notes-types'
/** One installed game in the storage view. `bytes` absent = unknown (no install path, or not measured yet). */
export interface StorageEntry { gameId: string; title: string; store: Store; lastPlayed?: number; bytes?: number; installPath?: string; measuredAt?: number; /** The size is a lower bound. */ partial: boolean; /** `manifest` = Steam's own figure. */ source?: 'manifest' | 'scan'; /** What "Uninstall..." hands off to; absent = no hand-off for this game. */ uninstall?: { routeIndex: number; launcher: string; kind: 'uninstall' | 'library' | 'apps' } }
export interface StorageReport { entries: StorageEntry[]; totalBytes: number }
export interface StorageProgress { done: number; total: number; title: string }
/** A game first discovered recently and not yet dismissed from the "Recently added" shelf. */
export interface RecentGame { gameId: string; addedAt: number }
/** Statistics (`stats_summary`). Playtime is in seconds; percents are 0 to 100. */
export interface StatsTopGame { gameId: string; title: string; /** What counts: the larger of tracked and store-reported time. */ seconds: number; trackedSeconds: number; storeSeconds?: number; /** The store `storeSeconds` came from, e.g. "Steam". */ storeSource?: string; percent: number; store: string; cover?: string }
export interface StatsSummary {
  includeHidden: boolean
  counts: { all: number; installed: number; notInstalled: number; /** Hidden games in the whole library. */ hidden: number; favorite: number }
  playtime: { totalSeconds: number; /** Over the games with time above zero. */ averageSeconds: number; playedGames: number; /** The part of the total that comes from a store's figure. */ storeSeconds: number }
  installSize: { bytes: number; known: number; unknown: number; partial: number }
  completion: { played: number; notPlayed: number }
  top: StatsTopGame[]
  library: Array<{ name: string; count: number; percent: number }>
  /** Stores whose reported time is part of the figures. */
  storeSources: string[]
}
export interface LobbyAPI {
  window: { minimize(): Promise<void>; toggleMaximize(): Promise<void>; close(): Promise<void>; isMaximized(): Promise<boolean> }
  library: { list(): Promise<Game[]>; scan(): Promise<LibraryStats>; launch(id: string, route?: number): Promise<LaunchOutcome>; continueLaunch(id: string, pendingId: string): Promise<LaunchOutcome>; cancelLaunch(pendingId: string): Promise<void>; pendingLaunch(pendingId: string): Promise<PendingLaunchStatus>; save(game: Partial<Game> & { id: string }): Promise<Game[]>; remove(id: string): Promise<Game[]>; linkPreview(gameIds: string[], title?: string): Promise<LinkPreview>; link(gameIds: string[], choices: LinkChoices, reviewId?: string): Promise<Game[]>; unlinkPreview(gameId: string, route: RouteRef): Promise<UnlinkPreview>; unlink(gameId: string, route: RouteRef): Promise<{ gameId: string; games: Game[] }>; renameRoute(gameId: string, route: RouteRef, name: string): Promise<Game[]>; setRouteArgs(gameId: string, route: RouteRef, args: string): Promise<Game[]>; /** The route's stored launch options won't run until they are confirmed again (they came from a backup or an older version, or its program changed). */ routeArgsPending(gameId: string, route: RouteRef): Promise<boolean>; /** Checks launch options text for a route without saving: what would run, or rejects with the reason. */ routeArgsCheck(gameId: string, route: RouteRef, args: string): Promise<ArgsPreview>; resetPlaytime(): Promise<Game[]>; setCover(id: string, dataUrl: string | null): Promise<Game[]>; chooseCover(id: string): Promise<Game[]>; chooseInstallFolder(id: string): Promise<void>; add(title: string, target: string): Promise<Game[]>; stores(): Promise<StoreStatus[]>; onChanged(callback: () => void): () => void
    /** Sets the emulator of a `rom` route (`routeIndex` into `game.routes`); `null` = automatic. The profile must exist; other route kinds are rejected. Returns the library. */
    setEmulator(gameId: string, routeIndex: number, emulatorId: string | null): Promise<Game[]>
    /**
     * Native file picker for a PC game (.exe/.lnk/.url) or a ROM file. Adds it (title from the file
     * name, editable with `save`) and returns its id, or null when cancelled. Adding a file already in
     * the library returns that game. A ROM's system comes from a system-named parent folder or a
     * unique extension; otherwise `system` is absent and an emulator must be chosen with `setEmulator`.
     * Shortcuts: a `.url` is accepted only when it opens a store launch link (the link is stored); a
     * `.lnk` to an existing `.exe` without arguments adds that `.exe`; any other `.lnk` REJECTS with a
     * message naming its target and arguments and creates a `shortcut` review item to confirm it
     * (`review.onChanged` fires).
     */
    addLocalGame(): Promise<AddedLocalGame | null> }
  settings: { get(): Promise<Settings>; save(settings: Partial<Settings>): Promise<Settings>; onChanged(callback: (settings: Settings) => void): () => void
    /** Shows Settings (inside the main window), optionally on a given section. */
    open(section?: SettingsSection): Promise<void>
    /** Rust asked the main window to show Settings (`section` is null for the page last shown). */
    onOpen(callback: (section: SettingsSection | null) => void): () => void }
  theme: { load(url: string): Promise<Settings>; reset(): Promise<Settings> }
  emulators: EmulatorsAPI
  launchers: LaunchersAPI
  roots: RootsAPI
  review: ReviewAPI
  health: HealthAPI
  backup: { export(): Promise<string | null>; import(): Promise<number> }
  plugins: { list(): Promise<PluginManifest[]>; install(): Promise<PluginManifest[]> }
  steamGridDb: SteamGridDbAPI
  steamOwned: SteamOwnedAPI
  accounts: AccountsAPI
  setup: SetupAPI
  tracking: TrackingAPI
  art: ArtAPI
  igdb: IgdbAPI
  wikimedia: WikimediaAPI
  screenshots: ScreenshotsAPI
  notes: NotesAPI
  updates: UpdatesAPI
  app: { quit(): Promise<void>; /** Fires the first time Game Lobby is brought back from the tray (one-time hint). */ onTrayHint(callback: () => void): () => void }
  /** Support tools. `report` is a redacted plain-text diagnostics report (no keys or tokens, user folder replaced); `openLogs` opens the fixed logs folder (the renderer passes no path). */
  diagnostics: { report(): Promise<string>; openLogs(): Promise<void> }
  /** Cached install sizes. `rescan` measures in the background (progress events) and rejects if one is running. `uninstall` only hands off to the game's launcher (never deletes files): the backend builds the link from the stored route. */
  storage: { list(): Promise<StorageReport>; rescan(): Promise<StorageReport>; uninstall(gameId: string, routeIndex: number): Promise<void>; onProgress(callback: (progress: StorageProgress) => void): () => void }
  /** The "Recently added" shelf. `dismiss()` without an id clears it. Both return the remaining shelf. */
  /** Read-only library statistics, computed by the backend. Hidden games are left out unless `includeHidden`. */
  stats: { summary(includeHidden: boolean): Promise<StatsSummary>; /** Games Steam reports time for, by game id (the others are absent). Refetch on `library.onChanged`. */ storeTimes(): Promise<Record<string, StoreTime>> }
  recent: { list(): Promise<RecentGame[]>; dismiss(gameId?: string): Promise<RecentGame[]> }
}
declare global { interface Window { lobby: LobbyAPI } }
