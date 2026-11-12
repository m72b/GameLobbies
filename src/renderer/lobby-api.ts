// Bridges the renderer's `window.lobby` API to the Rust backend's Tauri commands.
// Replaces the Electron preload script; the API shape is unchanged.
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { LobbyAPI, RunningGame, Settings, SteamGridDbFetchSummary } from '../shared/types'

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args)
  } catch (error) {
    // Rust commands reject with a plain message string; surface it as an Error.
    throw error instanceof Error ? error : new Error(String(error))
  }
}

function subscribe<T>(event: string, callback: (payload: T) => void): () => void {
  let disposed = false
  let unlisten: (() => void) | undefined
  void listen<T>(event, message => callback(message.payload)).then(off => {
    if (disposed) off()
    else unlisten = off
  })
  return () => {
    disposed = true
    unlisten?.()
  }
}

const api: LobbyAPI = {
  window: {
    minimize: () => call('window_minimize'),
    toggleMaximize: () => call('window_toggle_maximize'),
    close: () => call('window_close'),
    isMaximized: () => call('window_is_maximized')
  },
  library: {
    list: () => call('library_list'),
    scan: () => call('library_scan'),
    launch: (id, route) => call('library_launch', { id, route }),
    save: game => call('library_save', { game }),
    remove: id => call('library_remove', { id }),
    merge: (targetId, sourceId) => call('library_merge', { targetId, sourceId }),
    resetPlaytime: () => call('library_reset_playtime'),
    setCover: (id, dataUrl) => call('library_set_cover', { id, dataUrl }),
    chooseCover: id => call('library_choose_cover', { id }),
    chooseInstallFolder: id => call('library_open_install_folder', { id }),
    add: (title, target) => call('library_add', { title, target }),
    connections: () => call('library_connections'),
    saveConnections: input => call('library_save_connections', { input }),
    chooseLegendary: () => call('library_choose_legendary'),
    chooseGogDatabase: () => call('library_choose_gog_database'),
    onChanged: callback => subscribe('library:changed', () => callback())
  },
  settings: {
    get: () => call('settings_get'),
    save: settings => call('settings_save', { settings }),
    onChanged: callback => subscribe<Settings>('settings:changed', callback),
    open: () => call('settings_open')
  },
  theme: {
    load: url => call('theme_load', { url }),
    reset: () => call('theme_reset')
  },
  emulators: {
    list: () => call('emulators_list'),
    save: profile => call('emulators_save', { profile }),
    scan: () => call('emulators_scan')
  },
  backup: {
    export: () => call('backup_export'),
    import: () => call('backup_import')
  },
  plugins: {
    list: () => call('plugins_list'),
    install: () => call('plugins_install')
  },
  steamGridDb: {
    status: () => call('steamgriddb_status'),
    saveKey: key => call('steamgriddb_save_key', { key }),
    testKey: () => call('steamgriddb_test_key'),
    search: gameId => call('steamgriddb_search', { gameId }),
    apply: (gameId, url) => call('steamgriddb_apply', { gameId, url }),
    fetchMissing: () => call('steamgriddb_fetch_missing'),
    onFetched: callback => subscribe<SteamGridDbFetchSummary>('steamgriddb:fetched', callback)
  },
  tracking: {
    status: () => call('tracking_status'),
    onChanged: callback => subscribe<RunningGame[]>('tracking:changed', callback),
    sessions: (gameId, limit) => call('tracking_sessions', { gameId, limit }),
    stats: gameId => call('tracking_stats', { gameId }),
    setExcluded: (gameId, excluded) => call('tracking_set_excluded', { gameId, excluded })
  },
  art: {
    get: gameId => call('art_get', { gameId })
  },
  app: { quit: () => call('app_quit') }
}

window.lobby = api

/** Custom covers are stored as `lobby-cover://name`; WebView2 serves them from this origin. */
export function coverUrl(value: string) {
  return value.startsWith('lobby-cover://') ? `http://lobby-cover.localhost/${value.slice('lobby-cover://'.length)}` : value
}
