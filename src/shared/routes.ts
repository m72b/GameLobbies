import type { Game, GameRoute } from './types'

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
  if (!isEmulatorRoute(route)) return route.store
  const plain = emulatorSuffix(route, false)
  if (!plain) return route.store
  const clash = allRoutes.some(other => other !== route && isEmulatorRoute(other) && emulatorSuffix(other, false) === plain)
  return route.store + ' · ' + emulatorSuffix(route, clash)
}

/** Tooltip for a route label: the full (untruncated) label, plus the system name when known. */
export function routeTitle(route: GameRoute, allRoutes: GameRoute[] = [route], systemName?: string): string {
  const label = routeLabel(route, allRoutes)
  return isEmulatorRoute(route) && systemName ? label + ' (' + systemName + ')' : label
}

/** Whether a game has a Steam route with a numeric app id (Steam screenshots can be imported). */
export function isSteamGame(game: Pick<Game, 'routes'>): boolean {
  return game.routes.some(route => route.sourceId === 'steam' && /^\d{1,12}$/.test(route.appId ?? ''))
}
