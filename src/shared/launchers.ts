import type { CustomLauncher, Game, GameRoute } from './types'

/**
 * Pure helpers for custom launchers. The Rust side (`launchers.rs`) is the authority: these only
 * mirror its rules so the editor can explain problems before saving.
 */

/** `GameRoute.sourceId` prefix of a launcher's games. */
export const LAUNCHER_SOURCE_PREFIX = 'launcher:'
/** Library filter value for a launcher (`store` state in the main window). */
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

/** Swatches offered in the editor (any `#rrggbb` can be typed). */
export const LAUNCHER_SWATCHES = ['#4f8cff', '#75d18a', '#f2b84b', '#ef6b6b', '#b07cff', '#3fc6c6', '#ff8ac2', '#9aa3ad']

export function isHexColor(value: string): boolean {
  return /^#[0-9a-fA-F]{6}$/.test(value)
}

/** The launcher id of a launcher route, or undefined. */
export function launcherIdOf(route: Pick<GameRoute, 'sourceId'>): string | undefined {
  if (!route.sourceId.startsWith(LAUNCHER_SOURCE_PREFIX)) return undefined
  const id = route.sourceId.slice(LAUNCHER_SOURCE_PREFIX.length)
  return id || undefined
}

export function launcherFilter(id: string): string {
  return LAUNCHER_FILTER_PREFIX + id
}

/** The launcher a library filter value names, or undefined for a store filter. */
export function filterLauncherId(filter: string): string | undefined {
  return filter.startsWith(LAUNCHER_FILTER_PREFIX) ? filter.slice(LAUNCHER_FILTER_PREFIX.length) || undefined : undefined
}

/** Whether a game shows under a library filter: a store name, `All Games`, or a launcher. */
export function gameMatchesFilter(game: Pick<Game, 'store' | 'routes'>, filter: string): boolean {
  if (filter === 'All Games') return true
  const launcher = filterLauncherId(filter)
  if (launcher) return game.routes.some(route => launcherIdOf(route) === launcher)
  return game.store === filter
}

/** The launcher of a route, when it still exists. */
export function launcherOfRoute(route: Pick<GameRoute, 'sourceId'>, launchers: readonly CustomLauncher[]): CustomLauncher | undefined {
  const id = launcherIdOf(route)
  return id ? launchers.find(item => item.id === id) : undefined
}

/** The launcher a game is shown under: its first launcher route's (existing) launcher. */
export function gameLauncher(game: Pick<Game, 'routes'>, launchers: readonly CustomLauncher[]): CustomLauncher | undefined {
  for (const route of game.routes) {
    const launcher = launcherOfRoute(route, launchers)
    if (launcher) return launcher
  }
  return undefined
}

/** The store name to show for a game: the launcher's name for a Custom game run by a launcher. */
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
  const bad = [...id].find(c => !/^[A-Za-z0-9 ._:@#()'&+,!-]$/.test(c))
  if (bad) return 'It can’t contain “' + bad + '”. Use letters, digits, spaces and . _ : @ # ( ) \' & + , ! -'
  if (id.startsWith('-')) return 'It can’t start with “-” (the launcher would read it as an option).'
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

/** One argument per entry, quoted when it has spaces, for showing a preview command line. */
export function formatArgv(argv: readonly string[]): string {
  return argv.map(arg => arg === '' || /[\s"]/.test(arg) ? '"' + arg.replace(/"/g, '\\"') + '"' : arg).join(' ')
}

/** An example target for the preview, by target kind. */
export function sampleTarget(kind: CustomLauncher['targetKind']): string {
  return kind === 'identifier' ? 'game-id' : kind === 'folder' ? 'D:\\Games\\My Game' : 'D:\\Games\\My Game\\game.file'
}
