import type { AssignTarget, CustomLauncher, Game, GameRoute, LauncherStartMode, LauncherTargetKind, LibraryRoot, LocalScanSummary } from './types'

/**
 * Pure helpers for custom launchers. The Rust side (`launchers.rs`) is the authority: these only
 * mirror its rules so the editor can explain problems before saving.
 */

export const LAUNCHER_SOURCE_PREFIX = 'launcher:'
export const LAUNCHER_FILTER_PREFIX = 'launcher:'
export const TARGET_PLACEHOLDER = '{target}'
export const MAX_IDENTIFIER_CHARS = 200

/** Built-in icon names (lucide, kebab-case). Keep in sync with `BUILTIN_ICONS` in launchers.rs. */
export const LAUNCHER_ICONS = [
  'gamepad-2', 'joystick', 'rocket', 'swords', 'sword', 'shield', 'crown', 'castle',
  'ghost', 'skull', 'bot', 'puzzle', 'dices', 'trophy', 'star', 'heart',
  'flame', 'zap', 'car', 'terminal', 'code', 'wrench', 'package', 'folder'
] as const
export type LauncherIconName = typeof LAUNCHER_ICONS[number]

export const LAUNCHER_SWATCHES = ['#4f8cff', '#75d18a', '#f2b84b', '#ef6b6b', '#b07cff', '#3fc6c6', '#ff8ac2', '#9aa3ad']

export function isHexColor(value: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(value)
}

export function launcherIdOf(route: Pick<GameRoute, 'sourceId'>): string | undefined {
  if (!route.sourceId.startsWith(LAUNCHER_SOURCE_PREFIX)) return undefined
  const id = route.sourceId.slice(LAUNCHER_SOURCE_PREFIX.length)
  return id || undefined
}

/**
 * The launcher a route is run by (`launcher:<id>`) or labelled with (`launcherId`, a local or ROM
 * route found in a launcher's games folder), or undefined.
 */
export function routeLauncherId(route: Pick<GameRoute, 'sourceId'> & { launcherId?: string }): string | undefined {
  return launcherIdOf(route) ?? (route.launcherId || undefined)
}

export function launcherFilter(id: string): string {
  return LAUNCHER_FILTER_PREFIX + id
}

export function filterLauncherId(filter: string): string | undefined {
  return filter.startsWith(LAUNCHER_FILTER_PREFIX) ? filter.slice(LAUNCHER_FILTER_PREFIX.length) || undefined : undefined
}

export function gameMatchesFilter(game: Pick<Game, 'store' | 'routes'>, filter: string): boolean {
  if (filter === 'All Games') return true
  const launcher = filterLauncherId(filter)
  if (launcher) return game.routes.some(route => routeLauncherId(route) === launcher)
  return game.store === filter
}

export function launcherOfRoute(route: Pick<GameRoute, 'sourceId'> & { launcherId?: string }, launchers: readonly CustomLauncher[]): CustomLauncher | undefined {
  const id = routeLauncherId(route)
  return id ? launchers.find(item => item.id === id) : undefined
}

export const TARGET_KIND_ORDER: readonly LauncherTargetKind[] = ['file', 'folder', 'identifier']

/** A launcher's allowed target kinds (`targetKinds`, or `targetKind` from older data). */
export function launcherKinds(launcher: Pick<CustomLauncher, 'targetKind'> & { targetKinds?: LauncherTargetKind[] }): LauncherTargetKind[] {
  const kinds = launcher.targetKinds?.length ? launcher.targetKinds : [launcher.targetKind]
  return TARGET_KIND_ORDER.filter(kind => kinds.includes(kind))
}

/**
 * Turns a target kind on or off, keeping the canonical order (file, folder, identifier). The last
 * kind can't be turned off (a launcher needs at least one), and `locked` kinds (used by its games)
 * stay on.
 */
export function toggleKind(kinds: readonly LauncherTargetKind[], kind: LauncherTargetKind, locked: readonly LauncherTargetKind[] = []): LauncherTargetKind[] {
  const on = kinds.includes(kind)
  if (on && (kinds.length === 1 || locked.includes(kind))) return TARGET_KIND_ORDER.filter(item => kinds.includes(item))
  const next = on ? kinds.filter(item => item !== kind) : [...kinds, kind]
  return TARGET_KIND_ORDER.filter(item => next.includes(item))
}

/** A launcher without a program: only a label for the games found in its folders, which run directly. */
export function isLabelOnly(launcher: Pick<CustomLauncher, 'executable'>): boolean {
  return !launcher.executable
}

/** How Play starts a launcher's games (`through` when absent: launchers saved before start modes). */
export function startModeOf(launcher: Pick<CustomLauncher, 'executable'> & { startMode?: LauncherStartMode }): LauncherStartMode {
  return !isLabelOnly(launcher) && launcher.startMode === 'openFirst' ? 'openFirst' : 'through'
}

export function opensFirst(launcher: Pick<CustomLauncher, 'executable'> & { startMode?: LauncherStartMode }): boolean {
  return startModeOf(launcher) === 'openFirst'
}

/**
 * The target kinds a launcher may use in a start mode: an "open first" launcher starts each game
 * from its own file, so identifiers are dropped (file is kept when nothing else is left).
 */
export function kindsForMode(kinds: readonly LauncherTargetKind[], mode: LauncherStartMode): LauncherTargetKind[] {
  if (mode !== 'openFirst') return TARGET_KIND_ORDER.filter(kind => kinds.includes(kind))
  const next = TARGET_KIND_ORDER.filter(kind => kind !== 'identifier' && kinds.includes(kind))
  return next.length ? next : ['file']
}

/** Whether a launcher's games folders can be scanned: no program, or it takes files or folders. */
export function canScanFolders(launcher: Pick<CustomLauncher, 'executable' | 'targetKind'> & { targetKinds?: LauncherTargetKind[] }): boolean {
  return isLabelOnly(launcher) || launcherKinds(launcher).some(kind => kind !== 'identifier')
}

/** A launcher's games folders. */
export function launcherRoots(roots: readonly LibraryRoot[], launcherId: string): LibraryRoot[] {
  return roots.filter(root => root.launcherId === launcherId)
}

/** Library folders proper (launcher games folders are listed with their launcher). */
export function libraryRoots(roots: readonly LibraryRoot[]): LibraryRoot[] {
  return roots.filter(root => !root.launcherId)
}

const plural = (n: number, one: string, many = one + 's') => n + ' ' + (n === 1 ? one : many)

/**
 * The end-of-scan summary as label/value lines: games added, routes added to existing games,
 * emulators discovered, ROMs matched, items needing review, skipped items and errors.
 */
export function scanSummaryLines(summary: LocalScanSummary): Array<[string, string]> {
  const g = summary.games
  const errors = summary.roots.filter(root => !root.ok).length + g.failed
  return [
    ['Games added', String(g.added)],
    ['Routes added to existing games', String(g.routesAdded ?? 0)],
    ['Emulators discovered', String(summary.emulators.added) + (summary.emulators.updated ? ' (' + summary.emulators.updated + ' updated)' : '')],
    ['ROMs matched to an emulator', String(g.romsMatched ?? 0)],
    ['Needs review', summary.review === 0 ? 'Nothing' : plural(summary.review, 'item')],
    ['Skipped', plural(g.skipped, 'item')],
    ['Errors', errors === 0 ? 'None' : plural(errors, 'error')]
  ]
}

export function gameLauncher(game: Pick<Game, 'routes'>, launchers: readonly CustomLauncher[]): CustomLauncher | undefined {
  for (const route of game.routes) {
    const launcher = launcherOfRoute(route, launchers)
    if (launcher) return launcher
  }
  return undefined
}

export function displayStore(game: Pick<Game, 'store' | 'routes'>, launchers: readonly CustomLauncher[]): string {
  if (game.store !== 'Custom') return game.store
  return gameLauncher(game, launchers)?.name ?? game.store
}

/**
 * Mirrors Rust's `check_identifier`: an error message, or '' when the identifier is fine. It is
 * passed to the launcher as one argument.
 */
export function identifierProblem(raw: string): string {
  if (/[\u0000-\u001f\u007f-\u009f]/.test(raw)) return 'No tabs, line breaks or other control characters.'
  const id = raw.replace(/^ +| +$/g, '')
  if (!id) return 'Enter the game’s identifier.'
  if ([...id].length > MAX_IDENTIFIER_CHARS) return 'At most ' + MAX_IDENTIFIER_CHARS + ' characters.'
  if (id.includes('\\') || id.includes('/')) return 'No “\\” or “/”. To use a file or folder, set the launcher to File or Folder.'
  const bad = [...id].find(c => !/^[A-Za-z0-9 ._:@#()'+,!-]$/.test(c))
  if (bad) return 'It can’t contain “' + bad + '”. Use letters, digits, spaces and . _ : @ # ( ) \' + , ! -'
  if (id.startsWith('-')) return 'It can’t start with “-” (the launcher would read it as an option).'
  if (id.startsWith('@')) return 'It can’t start with “@” (the launcher could read it as a file of more arguments).'
  if (/^[A-Za-z]:/.test(id)) return 'It can’t start like a drive (“C:”): the launcher could read it as a file path.'
  if (id.includes('..')) return 'It can’t contain “..”.'
  return ''
}

/**
 * Splits a template like Rust's `tokenize_template`: unquoted whitespace separates arguments,
 * "..." and '...' group text (quotes removed), backslashes are literal. Null when a quote is
 * left open.
 */
export function tokenizeTemplate(template: string): string[] | null {
  const tokens: string[] = []
  let current = ''
  let inToken = false
  let quote: string | null = null
  for (const c of template) {
    if (quote) {
      if (c === quote) quote = null
      else current += c
    } else if (c === '"' || c === "'") {
      quote = c
      inToken = true
    } else if (/\s/.test(c)) {
      if (inToken) { tokens.push(current); current = ''; inToken = false }
    } else {
      current += c
      inToken = true
    }
  }
  if (quote) return null
  if (inToken) tokens.push(current)
  return tokens
}

/**
 * The arguments a launch would pass (the program excluded), like Rust's `build_argv`: every
 * `{target}` argument becomes exactly the target, and the target is appended when the template
 * has none. Null when the template can't be used (open quote, or `{target}` glued to other text).
 */
export function previewArgv(template: string, target: string): string[] | null {
  const tokens = tokenizeTemplate(template)
  if (!tokens) return null
  let used = false
  const argv: string[] = []
  for (const token of tokens) {
    if (token === TARGET_PLACEHOLDER) { argv.push(target); used = true }
    else if (token.includes('{') || token.includes('}')) return null
    else argv.push(token)
  }
  if (!used) argv.push(target)
  return argv
}

export function formatArgv(argv: readonly string[]): string {
  return argv.map(arg => arg === '' || /[\s"]/.test(arg) ? '"' + arg.replace(/"/g, '\\"') + '"' : arg).join(' ')
}

export function kindLabel(kind: LauncherTargetKind): string {
  return kind === 'file' ? 'File' : kind === 'folder' ? 'Folder' : 'Identifier'
}

export function sampleTarget(kind: CustomLauncher['targetKind']): string {
  return kind === 'identifier' ? 'game-id' : kind === 'folder' ? 'D:\\Games\\My Game' : 'D:\\Games\\My Game\\game.file'
}

export function isAssignedRoute(route: Pick<GameRoute, 'assigned'>): boolean {
  return route.assigned === true
}

export function assignedLauncherIds(game: Pick<Game, 'routes'>): string[] {
  const ids: string[] = []
  for (const route of game.routes) {
    const id = isAssignedRoute(route) ? routeLauncherId(route) : undefined
    if (id && !ids.includes(id)) ids.push(id)
  }
  return ids
}

/** How an assignment target reads in the game page's launcher picker: `File · hk.exe`, `Store id · 400 (Steam)`. */
export function assignTargetLabel(target: AssignTarget, routes: readonly Pick<GameRoute, 'store'>[]): string {
  const base = target.target.split(/[\\/]/).filter(Boolean).pop() ?? target.target
  const store = routes[target.routeIndex]?.store
  if (target.kind === 'identifier') return 'Store id · ' + target.target + (store ? ' (' + store + ')' : '')
  return (target.kind === 'file' ? 'File · ' : 'Folder · ') + base
}

/** What "Link to launcher" says once a game is linked to `launcher` (its name cut to 64 characters). */
export function linkedMessage(launcher: Pick<CustomLauncher, 'name' | 'executable'> & { startMode?: LauncherStartMode }): string {
  const name = launcher.name.length > 64 ? launcher.name.slice(0, 63) + '…' : launcher.name
  return isLabelOnly(launcher)
    ? 'Listed under ' + name + '. It keeps starting the same way.'
    : opensFirst(launcher)
      ? 'Linked to ' + name + '. Play now opens ' + name + ' first, then starts the game from its own file.'
      : 'Linked to ' + name + '. Play now starts it through ' + name + '.'
}

export function playsWithText(launcher: Pick<CustomLauncher, 'name' | 'executable'> & { startMode?: LauncherStartMode }): string {
  return opensFirst(launcher) ? 'Opens ' + launcher.name + ' first' : 'Plays through ' + launcher.name
}

/**
 * The launcher a Play button runs through (a `launcher:<id>` route of a launcher with a program),
 * to name it on the button; undefined for every other route.
 */
export function playLauncher(route: Pick<GameRoute, 'sourceId'> | undefined, launchers: readonly CustomLauncher[]): CustomLauncher | undefined {
  const id = route ? launcherIdOf(route) : undefined
  const launcher = id ? launchers.find(item => item.id === id) : undefined
  return launcher && !isLabelOnly(launcher) ? launcher : undefined
}
