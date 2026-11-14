// Bridges the renderer's `window.lobby` API to the Rust backend's Tauri commands.
// Replaces the Electron preload script; the API shape is unchanged.
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type { ArtChangedEvent, StorageProgress, IgdbBulkProgress, IgdbMetadataEvent, LobbyAPI, LocalScanProgress, LocalScanSummary, RunningGame, Settings, SettingsSection, SteamGridDbFetchSummary, UpdateProgress } from '../shared/types'

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
    stores: () => call('library_stores'),
    onChanged: callback => subscribe('library:changed', () => callback()),
    setEmulator: (gameId, routeIndex, emulatorId) => call('library_set_emulator', { gameId, routeIndex, emulatorId }),
    addLocalGame: () => call('library_add_local_game')
  },
  settings: {
    get: () => call('settings_get'),
    save: settings => call('settings_save', { settings }),
    onChanged: callback => subscribe<Settings>('settings:changed', callback),
    open: section => call('settings_open', { section: section ?? null }),
    onSection: callback => subscribe<SettingsSection>('settings:section', callback)
  },
  theme: {
    load: url => call('theme_load', { url }),
    reset: () => call('theme_reset')
  },
  emulators: {
    list: () => call('emulators_list'),
    save: profile => call('emulators_save', { profile }),
    remove: id => call('emulators_remove', { id }),
    scan: () => call('emulators_scan'),
    catalog: () => call('emulators_catalog'),
    ignored: () => call('emulators_ignored'),
    unignore: path => call('emulators_unignore', { path }),
    browseExecutable: () => call('emulators_browse_executable')
  },
  roots: {
    list: () => call('roots_list'),
    add: (kind, path) => call('roots_add', { kind, path }),
    browse: kind => call('roots_browse', { kind }),
    setEnabled: (id, enabled) => call('roots_set_enabled', { id, enabled }),
    remove: id => call('roots_remove', { id }),
    rescan: (id, options) => call('roots_rescan', { id: id ?? null, full: options?.full ?? false }),
    onProgress: callback => subscribe<LocalScanProgress>('library-scan:progress', callback),
    onFinished: callback => subscribe<LocalScanSummary>('library-scan:finished', callback)
  },
  review: {
    list: includeResolved => call('review_list', { includeResolved: includeResolved ?? false }),
    resolve: (id, resolution) => call('review_resolve', { id, resolution }),
    ignored: () => call('review_ignored'),
    unignore: path => call('review_unignore', { path }),
    onChanged: callback => subscribe('review:changed', () => callback())
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
    search: (gameId, page) => call('steamgriddb_search', { gameId, page: page ?? 0 }),
    searchHeroes: (gameId, page) => call('steamgriddb_search_heroes', { gameId, page: page ?? 0 }),
    searchLogos: (gameId, page) => call('steamgriddb_search_logos', { gameId, page: page ?? 0 }),
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
    get: gameId => call('art_get', { gameId }),
    setHero: (gameId, url) => call('art_set_hero', { gameId, url }),
    setLogo: (gameId, url) => call('art_set_logo', { gameId, url }),
    chooseHeroFile: gameId => call('art_choose_hero_file', { gameId }),
    chooseLogoFile: gameId => call('art_choose_logo_file', { gameId }),
    setLogoLayout: (gameId, position, scale) => call('art_set_logo_layout', { gameId, position, scale }),
    onChanged: callback => subscribe<ArtChangedEvent>('art:changed', callback)
  },
  screenshots: {
    list: gameId => call('screenshots_list', { gameId }),
    add: gameId => call('screenshots_add', { gameId }),
    importSteam: gameId => call('screenshots_import_steam', { gameId }),
    remove: id => call('screenshots_remove', { id }),
    setAsHero: id => call('screenshots_set_as_hero', { id }),
    onChanged: callback => subscribe<{ gameId: string }>('screenshots:changed', callback)
  },
  igdb: {
    status: () => call('igdb_status'),
    saveCredentials: (clientId, clientSecret) => call('igdb_save_credentials', { clientId, clientSecret }),
    clearCredentials: () => call('igdb_clear_credentials'),
    test: () => call('igdb_test'),
    get: gameId => call('igdb_get', { gameId }),
    search: (gameId, query) => call('igdb_search', { gameId, query: query ?? null }),
    choose: (gameId, igdbId) => call('igdb_choose', { gameId, igdbId }),
    resetMatch: gameId => call('igdb_reset_match', { gameId }),
    fetchAll: () => call('igdb_fetch_all'),
    cancelFetchAll: () => call('igdb_cancel_fetch_all'),
    progress: () => call('igdb_progress'),
    onMetadata: callback => subscribe<IgdbMetadataEvent>('igdb:metadata', callback),
    onProgress: callback => subscribe<IgdbBulkProgress>('igdb:progress', callback)
  },
  updates: {
    check: () => call('updates_check'),
    autoCheck: () => call('updates_auto_check'),
    last: () => call('updates_last'),
    install: () => call('updates_install'),
    onProgress: callback => subscribe<UpdateProgress>('updates:progress', callback),
    onReady: callback => subscribe('updates:ready', () => callback())
  },
  app: { quit: () => call('app_quit') },
  storage: {
    list: () => call('storage_list'),
    rescan: () => call('storage_rescan'),
    uninstall: (gameId, routeIndex) => call('storage_uninstall', { gameId, routeIndex }),
    onProgress: callback => subscribe<StorageProgress>('storage:progress', callback)
  },
  recent: {
    list: () => call('recent_list'),
    dismiss: gameId => call('recent_dismiss', { gameId: gameId ?? null })
  }
}

window.lobby = api

/**
 * Custom covers are stored as `lobby-cover://name` and screenshots as `lobby-shot://dir/file`;
 * WebView2 serves them from these origins.
 */
export function coverUrl(value: string) {
  if (value.startsWith('lobby-cover://')) return `http://lobby-cover.localhost/${value.slice('lobby-cover://'.length)}`
  if (value.startsWith('lobby-shot://')) return `http://lobby-shot.localhost/${value.slice('lobby-shot://'.length)}`
  return value
}
