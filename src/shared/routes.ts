import type { GameRoute } from './types'

/**
 * Whether a route runs through an emulator, so the game page offers an emulator picker: ROM
 * routes (`sourceId` `rom`, store `Emulator`) only. PC games (`sourceId` `local`, system `pc`)
 * run directly and never get one, even though they carry a system id.
 */
export function isEmulatorRoute(route: Pick<GameRoute, 'sourceId' | 'store' | 'system'>): boolean {
  if (route.sourceId === 'local' || route.system === 'pc') return false
  return route.sourceId === 'rom' || route.store === 'Emulator'
}
