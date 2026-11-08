import { app } from 'electron'
import { existsSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join } from 'node:path'

/** Functions exported by native/ (Rust, Node-API). Keep in sync with native/src/lib.rs. */
export interface LobbyNative {
  runningProcessPaths(): string[]
}

let cached: LobbyNative | null | undefined

/**
 * Loads native/lobby_native.node once. Returns null when it is missing, fails to load,
 * or is disabled with LOBBY_DISABLE_NATIVE=1; callers then use their PowerShell fallbacks.
 * In a packaged app the file sits in app.asar.unpacked, and Electron redirects the require there.
 */
export function lobbyNative(): LobbyNative | null {
  if (cached !== undefined) return cached
  cached = null
  if (process.platform !== 'win32' || process.env.LOBBY_DISABLE_NATIVE === '1') return cached
  const file = join(app.getAppPath(), 'native', 'lobby_native.node')
  if (!existsSync(file) && !existsSync(file.replace(/app\.asar([\\/])/, 'app.asar.unpacked$1'))) return cached
  try {
    const loaded: unknown = createRequire(__filename)(file)
    if (loaded && typeof (loaded as LobbyNative).runningProcessPaths === 'function') cached = loaded as LobbyNative
    else console.warn('Native addon is missing expected exports; using fallbacks.')
  } catch (error) {
    console.warn('Native addon could not be loaded; using fallbacks.', error)
  }
  return cached
}
