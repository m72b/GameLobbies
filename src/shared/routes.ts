import type { CustomLauncher, Game, GameRoute, RouteRef } from './types'

/**
 * Whether a route runs through an emulator, so the game page offers an emulator picker: ROM
 * routes (`sourceId` `rom`, store `Emulator`) only. PC games (`sourceId` `local`, system `pc`)
 * run directly and never get one, even though they carry a system id.
 */
export function isEmulatorRoute(route: Pick<GameRoute, 'sourceId' | 'store' | 'system'>): boolean {
  if (route.sourceId === 'local' || route.system === 'pc') return false
  return route.sourceId === 'rom' || route.store === 'Emulator'
}

function emulatorSuffix(route: GameRoute, withFolder: boolean): string {
  const parts = (route.launchTarget ?? '').split(/[\\/]+/).filter(Boolean)
  const file = parts[parts.length - 1]
  if (!file) return ''
  const folder = parts.length > 1 ? parts[parts.length - 2] : ''
  return withFolder && folder ? folder + '/' + file : file
}

/**
 * The text that tells a game's launch routes apart. Non-emulator routes keep their store name. An
 * emulator route reads `Emulator · <ROM file name>` (extension kept, so `.iso` and `.chd` differ);
 * if two emulator routes would still read the same, the ROM's parent folder is added.
 */
export function routeLabel(route: GameRoute, allRoutes: GameRoute[] = [route]): string {
  if (route.name) return withRouteName(route.store, route)
  if (!isEmulatorRoute(route)) return route.store
  const plain = emulatorSuffix(route, false)
  if (!plain) return route.store
  const clash = allRoutes.some(other => other !== route && isEmulatorRoute(other) && emulatorSuffix(other, false) === plain)
  return route.store + ' · ' + emulatorSuffix(route, clash)
}

export function routeTitle(route: GameRoute, allRoutes: GameRoute[] = [route], systemName?: string): string {
  const label = routeLabel(route, allRoutes)
  return isEmulatorRoute(route) && systemName ? label + ' (' + systemName + ')' : label
}

export function isSteamGame(game: Pick<Game, 'routes'>): boolean {
  return game.routes.some(route => route.sourceId === 'steam' && /^\d{1,12}$/.test(route.appId ?? ''))
}

/** `base` followed by the route's own name, when it has one ("Xbox · Java Edition"). */
export function withRouteName(base: string, route: Pick<GameRoute, 'name'>): string {
  const name = route.name?.trim()
  return name ? base + ' · ' + name : base
}

/** How the backend finds a route again (`RouteRef`): its position plus its stored source and target. */
export function routeRef(route: Pick<GameRoute, 'sourceId' | 'launchTarget'>, index: number): RouteRef {
  return { index, sourceId: route.sourceId, launchTarget: route.launchTarget }
}

export type ArgsSupport = { ok: true; launcher?: string } | { ok: false; reason: string }

/** The user guide's section on launch options (opened in the browser; the only github.com page the app may open). */
export const LAUNCH_OPTIONS_HELP = 'https://github.com/m72b/GameLobbies/blob/main/docs/USER-GUIDE.md#launch-options'

/**
 * Whether a route can take launch options, mirroring the backend's `links::args_host` (which decides
 * and checks again): a local PC route that starts its own `.exe`, or a route of a custom launcher
 * with a program (`launcher` is its name: the options come after its own arguments). Store links,
 * Xbox apps, emulators and launchers without a program start the game themselves.
 */
export function argsSupport(route: Pick<GameRoute, 'sourceId' | 'store' | 'launchTarget' | 'emulatorId'>, launchers: readonly (Pick<CustomLauncher, 'id' | 'name' | 'executable'> & { startMode?: CustomLauncher['startMode'] })[] = []): ArgsSupport {
  if (route.sourceId.startsWith('launcher:')) {
    const launcher = launchers.find(item => 'launcher:' + item.id === route.sourceId)
    if (!launcher) return { ok: false, reason: 'This game’s launcher was deleted. Add the game again to another launcher.' }
    if (!launcher.executable) return { ok: false, reason: `${launcher.name} has no program, so this route can’t have launch options. Its games run directly: set options on the game’s own .exe route instead.` }
    // Opened first, the launcher gets nothing: the game starts from its own .exe, which takes the options.
    if (launcher.startMode !== 'openFirst') return { ok: true, launcher: launcher.name }
    const own = route.launchTarget.trim()
    if (!/^[a-z]:[\\/]/i.test(own) ||!/\.exe$/i.test(own)) return { ok: false, reason: `${launcher.name} is opened first, and then the game starts from its own file. Only a game started from its own .exe can have launch options here (not a shortcut, folder or ROM).` }
    return { ok: true }
  }
  if (route.sourceId === 'rom' || route.emulatorId) return { ok: false, reason: 'ROMs start with their emulator’s arguments. Change them under Settings → Emulators.' }
  if (route.sourceId !== 'local') return { ok: false, reason: `${route.store} starts this game itself, so Game Lobby can’t pass launch options to it. Set them in ${route.store} instead.` }
  const target = route.launchTarget.trim()
  if (!/^[a-z]:[\\/]/i.test(target) || !/\.exe$/i.test(target)) return { ok: false, reason: 'Only a game started from its own .exe can have launch options here (not a shortcut, folder or link).' }
  return { ok: true }
}

/** Stored launch options (a JSON list, or older plain text) as text for the editor. */
export function argsText(launchArgs: string | undefined): string {
  if (!launchArgs) return ''
  let argv: unknown
  try { argv = JSON.parse(launchArgs) } catch { return launchArgs }
  if (!Array.isArray(argv)) return launchArgs
  return argv.map(arg => {
    const text = String(arg)
    if (text && !/[\s"']/.test(text)) return text
    return text.includes('"') ? "'" + text + "'" : '"' + text + '"'
  }).join(' ')
}