import { useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import Fuse from 'fuse.js'
import { useVirtualizer } from '@tanstack/react-virtual'
import { Boxes, Clock3, CloudDownload, EyeOff, Gamepad2, Heart, Library, ListFilter, MoreHorizontal, Pencil, Play, Plus, Search, Settings2, Star, X } from 'lucide-react'
import { StoreIcon } from './store-icons'
import type { Game, RunningGame, Settings, Store as StoreName, SteamGridDbCover, UpdateInfo } from '../shared/types'
import { defaultSettings } from '../shared/defaults'
import { applyTheme } from './theme'
import { coverStyle } from './cover'
import { CoverLayer, useCover } from './poster'
import { CoverTiles, LoadMore, pickerCovers, useSgdbPages } from './cover-picker'
import { GamePage, type GamePageActions } from './game-page'
import { elapsedSeconds, formatClock, useNow } from './format'

type View = 'All Games' | 'Favorites' | 'Recently Played' | 'Hidden'
type SortBy = 'title' | 'lastPlayed' | 'playtime' | 'library'
const sortOptions: SortBy[] = ['title', 'lastPlayed', 'playtime', 'library']
const SORT_KEY = 'gameLobby.sortBy'
/** The library sort is remembered per viewer (browser storage may be unavailable). */
function storedSort(): SortBy {
  try {
    const value = window.localStorage.getItem(SORT_KEY)
    return sortOptions.includes(value as SortBy) ? value as SortBy : 'title'
  } catch { return 'title' }
}
function rememberSort(value: SortBy) {
  try { window.localStorage.setItem(SORT_KEY, value) } catch { /* not persisted */ }
}
type MenuAction = 'Play' | 'Edit metadata' | 'Set custom cover' | 'Find cover on SteamGridDB' | 'Hide' | 'Unhide' | 'Add to collection' | 'Link another game' | 'Open install folder' | 'Remove'

// Saves from the UI leave launch routes alone: program and file routes can only be changed by
// scans and the native pickers, and a stale copy would be refused.
const withoutRoutes = ({ routes: _routes, ...game }: Game): Omit<Game, 'routes'> => game
const isInstalled = (game: Game) => game.routes.some(route => route.installed === true)
const stores: StoreName[] = ['Steam', 'Epic Games', 'GOG', 'EA app', 'Battle.net', 'Xbox', 'Ubisoft', 'Emulator', 'itch.io', 'Amazon Games', 'Custom']

export default function App() {
  const [games, setGames] = useState<Game[]>([])
  const [settings, setSettings] = useState<Settings>(defaultSettings)
  const [query, setQuery] = useState('')
  const [store, setStore] = useState('All Games')
  const [view, setView] = useState<View>('All Games')
  const [sortBy, setSortByState] = useState<SortBy>(storedSort)
  const setSortBy = (value: SortBy) => { setSortByState(value); rememberSort(value) }
  const [openId, setOpenId] = useState<string | null>(null)
  const [running, setRunning] = useState<RunningGame[]>([])
  const [libraryVersion, setLibraryVersion] = useState(0)
  const savedScroll = useRef(0)
  const [linkingGame, setLinkingGame] = useState<Game | null>(null)
  const [linkQuery, setLinkQuery] = useState('')
  const [menu, setMenu] = useState<{ x: number; y: number; game: Game } | null>(null)
  const [scanBusy, setScanBusy] = useState(false)
  const [localBusy, setLocalBusy] = useState(false)
  const [toast, setToast] = useState('')
  const [updateNotice, setUpdateNotice] = useState<UpdateInfo | null>(null)
  const [adding, setAdding] = useState(false)
  const [editingGame, setEditingGame] = useState<Game | null>(null)
  const [editTitle, setEditTitle] = useState('')
  const [editGenre, setEditGenre] = useState('')
  const [editFavorite, setEditFavorite] = useState(false)
  const [sgdbGame, setSgdbGame] = useState<Game | null>(null)
  const [sgdbApplying, setSgdbApplying] = useState(false)
  const [editMode, setEditMode] = useState(false)
  const [collapsed, setCollapsed] = useState(true)
  const [gridWidth, setGridWidth] = useState(1200)
  const [focusedId, setFocusedId] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const filteredRef = useRef<Game[]>([])
  const focusedIdRef = useRef<string | null>(null)
  const colsRef = useRef(6)
  const scrollToRowRef = useRef<(row: number) => void>(() => {})

  const refresh = async () => {
    const [nextGames, nextSettings] = await Promise.all([window.lobby.library.list(), window.lobby.settings.get()])
    setGames(nextGames)
    setSettings(nextSettings)
  }
  const notice = (message: string) => {
    setToast(message)
    window.setTimeout(() => setToast(''), 2500)
  }
  const launch = async (game: Game, route = 0) => {
    try {
      await window.lobby.library.launch(game.id, route)
      await refresh()
      notice('Launching ' + game.title)
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not launch game')
    }
  }
  const openGame = (game: Game) => {
    savedScroll.current = scrollRef.current?.scrollTop ?? savedScroll.current
    setOpenId(game.id)
    setMenu(null)
  }
  const closeGame = () => setOpenId(null)
  const beginEdit = (game: Game) => {
    setEditingGame(game)
    setEditTitle(game.title)
    setEditGenre(game.genre || '')
    setEditFavorite(game.favorite)
    setMenu(null)
  }
  const saveEdit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!editingGame || !editTitle.trim()) return
    try {
      await window.lobby.library.save({ ...withoutRoutes(editingGame), title: editTitle.trim(), genre: editGenre.trim() || undefined, favorite: editFavorite })
      const nextGames = await window.lobby.library.list()
      setGames(nextGames)
      setEditingGame(null)
      notice('Game details saved')
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not save game details')
    }
  }
  const setCover = async (game: Game) => {
    await window.lobby.library.chooseCover(game.id)
    await refresh()
  }
  const sgdbSettingsKey = JSON.stringify([settings.steamGridDbStyles, settings.steamGridDbAnimated, settings.steamGridDbNsfw, settings.steamGridDbHumor])
  const sgdbPages = useSgdbPages('grid', sgdbGame?.id ?? '', sgdbSettingsKey)
  const sgdbCovers = useMemo(() => pickerCovers(sgdbPages.covers, settings.steamGridDbAnimated), [sgdbPages.covers, settings.steamGridDbAnimated])
  const startSgdb = sgdbPages.start
  const resetSgdb = sgdbPages.reset
  useEffect(() => { if (sgdbGame) void startSgdb(); else resetSgdb() }, [sgdbGame, startSgdb, resetSgdb])
  const findCover = async (game: Game) => {
    setMenu(null)
    setSgdbApplying(false)
    setSgdbGame(game)
  }
  const closeFindCover = () => { setSgdbGame(null); setSgdbApplying(false) }
  const applySgdbCover = async (cover: SteamGridDbCover) => {
    if (!sgdbGame || sgdbApplying) return
    const game = sgdbGame
    setSgdbApplying(true)
    try {
      const nextGames = await window.lobby.steamGridDb.apply(game.id, cover.url)
      setGames(nextGames)
      closeFindCover()
      notice('Cover updated')
    } catch (error) {
      setSgdbApplying(false)
      notice(error instanceof Error ? error.message : 'Could not apply cover')
    }
  }
  const linkAnotherGame = (game: Game) => {
    setLinkingGame(game)
    setLinkQuery('')
    setMenu(null)
  }
  const mergeIntoKeptGame = async (source: Game) => {
    if (!linkingGame) return
    try {
      const nextGames = await window.lobby.library.merge(linkingGame.id, source.id)
      setGames(nextGames)
      setLinkingGame(null)
      notice(`Linked ${source.title} to ${linkingGame.title}`)
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not link these games')
    }
  }
  const menuAction = async (action: MenuAction, game: Game) => {
    setMenu(null)
    if (action === 'Play') return launch(game)
    if (action === 'Edit metadata') return beginEdit(game)
    if (action === 'Set custom cover') return setCover(game)
    if (action === 'Find cover on SteamGridDB') return findCover(game)
    if (action === 'Link another game') return linkAnotherGame(game)
    if (action === 'Hide' || action === 'Unhide') {
      const nextGames = await window.lobby.library.save({ ...withoutRoutes(game), hidden: action === 'Hide' })
      setGames(nextGames)
      return
    }
    if (action === 'Open install folder') return window.lobby.library.chooseInstallFolder(game.id)
    if (action === 'Remove' && window.confirm('Remove ' + game.title + ' from your library?')) {
      await window.lobby.library.remove(game.id)
      setOpenId(current => current === game.id ? null : current)
      await refresh()
    }
  }
  const scan = async () => {
    setScanBusy(true)
    try {
      const result = await window.lobby.library.scan()
      await refresh()
      notice(result.errors?.length ? 'Imported ' + (result.imported || 0) + ' records · ' + result.errors[0] : 'Library updated · ' + result.total + ' games')
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Library scan failed')
    } finally {
      setScanBusy(false)
    }
  }
  const addLocalGame = async () => {
    setLocalBusy(true)
    try {
      const result = await window.lobby.library.addLocalGame()
      if (!result) return
      setGames(result.games)
      const added = result.games.find(game => game.id === result.gameId)
      if (added) openGame(added)
      notice(added ? 'Added ' + added.title : 'Game added')
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not add the game')
    } finally {
      setLocalBusy(false)
    }
  }
  const addGame = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    try {
      await window.lobby.library.add(String(data.get('title')), String(data.get('target')))
      setAdding(false)
      await refresh()
      notice('Game added')
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not add the game')
    }
  }

  useEffect(() => {
    void refresh()
    const offLibrary = window.lobby.library.onChanged(() => { setLibraryVersion(value => value + 1); void refresh() })
    void window.lobby.tracking.status().then(setRunning).catch(() => {})
    const offTracking = window.lobby.tracking.onChanged(setRunning)
    const offSettings = window.lobby.settings.onChanged(setSettings)
    const offSgdb = window.lobby.steamGridDb.onFetched(summary => { if (summary.updated > 0) notice('SteamGridDB added ' + summary.updated + ' cover' + (summary.updated === 1 ? '' : 's')) })
    const resize = () => setGridWidth(scrollRef.current?.clientWidth || window.innerWidth - 90)
    const observer = new ResizeObserver(resize)
    if (scrollRef.current) observer.observe(scrollRef.current)
    resize()
    const key = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement
      const typing = ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        searchRef.current?.focus()
        return
      }
      if (event.key === 'Escape') {
        if (!menu && !adding && !editingGame && !sgdbGame && !linkingGame) setOpenId(null)
        setMenu(null)
        setAdding(false)
        setEditingGame(null)
        setSgdbGame(null)
        return
      }
      if (!typing && !openId && ['ArrowRight', 'ArrowLeft', 'ArrowDown', 'ArrowUp'].includes(event.key)) {
        event.preventDefault()
        const list = filteredRef.current
        if (!list.length) return
        let index = list.findIndex(game => game.id === focusedIdRef.current)
        if (index < 0) index = 0
        else {
          const delta = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : event.key === 'ArrowDown' ? colsRef.current : -colsRef.current
          index = Math.max(0, Math.min(list.length - 1, index + delta))
        }
        const next = list[index]
        setFocusedId(next.id)
        scrollToRowRef.current(Math.floor(index / colsRef.current))
        return
      }
      if (event.key === 'Enter' && !typing) {
        const game = filteredRef.current.find(item => item.id === focusedIdRef.current)
        if (game && !openId && !menu && !adding && !editingGame && !target.closest('button')) {
          event.preventDefault()
          openGame(game)
        }
      }
    }
    window.addEventListener('keydown', key)
    return () => {
      offLibrary()
      offTracking()
      offSettings()
      offSgdb()
      observer.disconnect()
      window.removeEventListener('keydown', key)
    }
  }, [openId, menu, adding, editingGame, sgdbGame, linkingGame])

  useEffect(() => { applyTheme(settings.theme, settings) }, [settings])
  useEffect(() => {
    // Startup update check (the backend skips it when turned off or done in the last 24 h).
    const timer = window.setTimeout(() => {
      void window.lobby.updates.autoCheck().then(info => { if (info?.available) setUpdateNotice(info) }).catch(() => undefined)
    }, 10_000)
    return () => window.clearTimeout(timer)
  }, [])
  const openGameData = openId ? games.find(game => game.id === openId) : undefined
  const pageOpen = !!openGameData
  const wasPageOpen = useRef(false)
  useLayoutEffect(() => {
    if (wasPageOpen.current && !pageOpen && scrollRef.current) {
      const top = savedScroll.current
      scrollRef.current.scrollTop = top
      requestAnimationFrame(() => { if (scrollRef.current) scrollRef.current.scrollTop = top })
    }
    wasPageOpen.current = pageOpen
  }, [pageOpen])
  useEffect(() => {
    const resize = () => setGridWidth(scrollRef.current?.clientWidth || window.innerWidth - 90)
    if (!scrollRef.current) return
    const observer = new ResizeObserver(resize)
    observer.observe(scrollRef.current)
    resize()
    return () => observer.disconnect()
  }, [pageOpen])
  const runningById = useMemo(() => new Map(running.map(item => [item.gameId, item])), [running])
  const nowPlaying = useMemo(() => {
    const item = running.find(entry => entry.state === 'running') || running[0]
    const game = item && games.find(entry => entry.id === item.gameId)
    return item && game ? { item, game } : null
  }, [running, games])

  const setInstalledFilter = async (value: boolean) => {
    try { setSettings(await window.lobby.settings.save({ showOnlyInstalled: value })) }
    catch (error) { notice(error instanceof Error ? error.message : 'Settings could not be saved') }
  }
  const fuzzy = useMemo(() => new Fuse(games, { keys: ['title', 'genre', 'store'], threshold: 0.38, ignoreLocation: true }), [games])
  const onlyInstalled = settings.showOnlyInstalled
  const filtered = useMemo(() => {
    const candidates = query.trim() ? fuzzy.search(query).map(result => result.item) : games
    return candidates.filter(game =>
      (!onlyInstalled || isInstalled(game)) &&
      (view === 'Hidden' ? game.hidden : !game.hidden) &&
      (store === 'All Games' || game.store === store) &&
      (view !== 'Favorites' || game.favorite) &&
      (view !== 'Recently Played' || !!game.lastPlayed)
    )
  }, [games, query, store, view, fuzzy, onlyInstalled])
  const railStores = useMemo(() => stores.filter(item => item === store || games.some(game => game.store === item && !game.hidden && (!onlyInstalled || isInstalled(game)))).slice(0, 8), [games, store, onlyInstalled])
  const ordered = useMemo(() => {
    if (query.trim()) return filtered
    return [...filtered].sort((a, b) =>
      sortBy === 'lastPlayed' ? (b.lastPlayed || 0) - (a.lastPlayed || 0) :
        sortBy === 'playtime' ? b.playtimeMinutes - a.playtimeMinutes :
          sortBy === 'library' ? a.store.localeCompare(b.store) || a.title.localeCompare(b.title) : a.title.localeCompare(b.title)
    )
  }, [filtered, query, sortBy])
  const cols = Math.max(2, Math.floor(gridWidth / (settings.posterWidth + Number.parseInt(settings.theme.gap || '9'))))
  filteredRef.current = ordered
  focusedIdRef.current = focusedId
  colsRef.current = cols
  const rows = useMemo(() => Array.from({ length: Math.ceil(ordered.length / cols) }, (_, index) => ordered.slice(index * cols, (index + 1) * cols)), [ordered, cols])
  const virtual = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => settings.posterWidth * 1.5 + 62, overscan: 3 })
  scrollToRowRef.current = row => virtual.scrollToIndex(row, { align: 'auto' })
  const visibleRowsKey = virtual.getVirtualItems().map(row => row.index).join(',')
  useEffect(() => {
    if (!focusedId) return
    const escaped = CSS.escape(focusedId)
    scrollRef.current?.querySelector<HTMLElement>('[data-game-id="' + escaped + '"]')?.focus({ preventScroll: true })
  }, [focusedId, visibleRowsKey])
  useEffect(() => {
    if (focusedId && !ordered.some(game => game.id === focusedId)) setFocusedId(null)
  }, [ordered, focusedId])

  const menuOptions: Array<{ action: MenuAction; label: string; disabled?: boolean }> = [
    { action: 'Play', label: 'Play' },
    { action: 'Edit metadata', label: 'Edit metadata' },
    { action: 'Set custom cover', label: 'Set custom cover' },
    { action: 'Find cover on SteamGridDB', label: 'Find cover on SteamGridDB' },
    { action: 'Link another game', label: 'Link another game' },
    { action: menu?.game.hidden ? 'Unhide' : 'Hide', label: menu?.game.hidden ? 'Unhide' : 'Hide' },
    { action: 'Add to collection', label: 'Add to collection · Coming soon', disabled: true },
    { action: 'Open install folder', label: 'Open install folder' },
    { action: 'Remove', label: 'Remove' }
  ]

  const pageActions: GamePageActions = {
    back: closeGame,
    play: (game, route) => void launch(game, route),
    edit: beginEdit,
    setCover: game => void setCover(game).catch(error => notice(error instanceof Error ? error.message : 'Could not set cover')),
    findCover: game => void findCover(game),
    link: linkAnotherGame,
    toggleHidden: game => void menuAction(game.hidden ? 'Unhide' : 'Hide', game),
    toggleFavorite: game => void window.lobby.library.save({ ...withoutRoutes(game), favorite: !game.favorite }).then(setGames).catch(error => notice(error instanceof Error ? error.message : 'Could not update favorite')),
    openFolder: game => void window.lobby.library.chooseInstallFolder(game.id).catch(error => notice(error instanceof Error ? error.message : 'Could not open install folder')),
    remove: game => void menuAction('Remove', game),
    setEmulator: async (game, routeIndex, emulatorId) => { setGames(await window.lobby.library.setEmulator(game.id, routeIndex, emulatorId)) }
  }

  return <div className="app-shell">
    <header className="titlebar">
      <div className="brand">Game Lobby</div>{nowPlaying && !(pageOpen && openId === nowPlaying.game.id) && <NowPlaying game={nowPlaying.game} entry={nowPlaying.item} onOpen={() => openGame(nowPlaying.game)} />}<div className="drag-space" data-tauri-drag-region />
      <button className="title-action" onClick={() => window.lobby.window.minimize()} aria-label="Minimize">—</button>
      <button className="title-action" onClick={() => window.lobby.window.toggleMaximize()} aria-label="Maximize">□</button>
      <button className="title-action close" onClick={() => window.lobby.window.close()} aria-label="Close">×</button>
    </header>
    <aside className={'rail ' + (collapsed ? 'compact' : '')}>
      <button className="rail-button" onClick={() => setCollapsed(!collapsed)} title="Menu"><Boxes /></button>
      <button className="rail-button search-button" onClick={() => searchRef.current?.focus()} title="Search"><Search /></button>
      <div className="rail-sep" />
      <button className={'rail-button ' + (view === 'All Games' ? 'active' : '')} onClick={() => { setView('All Games'); setStore('All Games') }} title="All games"><Library /></button>
      <button className={'rail-button ' + (view === 'Favorites' ? 'active' : '')} onClick={() => setView('Favorites')} title="Favorites"><Heart /></button>
      <button className={'rail-button ' + (view === 'Recently Played' ? 'active' : '')} onClick={() => setView('Recently Played')} title="Recently played"><Clock3 /></button>
      <button className={'rail-button ' + (view === 'Hidden' ? 'active' : '')} onClick={() => setView('Hidden')} title="Hidden games"><EyeOff /></button>
      <div className="rail-sep" />
      <div className="rail-stores">{railStores.map(item =>
        <button key={item} className={'rail-button mini ' + (store === item ? 'active' : '')} title={item} onClick={() => { setStore(store === item ? 'All Games' : item); setView('All Games') }}><StoreIcon store={item} size={16} /></button>
      )}<button className="rail-button mini" title="Add source" onClick={() => window.lobby.settings.open()}><Plus /></button></div>
      <div className="rail-bottom">
        <button className="rail-button" title="Export library" onClick={() => void window.lobby.backup.export()}><CloudDownload /></button>
        <button className={'rail-button ' + (editMode ? 'active' : '')} title="Edit mode" onClick={() => setEditMode(!editMode)}><Pencil /></button>
        <button className="rail-button" title="Settings" onClick={() => window.lobby.settings.open()}><Settings2 /></button>
        <button className="rail-button" title="Quit" onClick={() => window.lobby.app.quit()}><X /></button>
      </div>
    </aside>
    <main className={'main' + (pageOpen ? ' page-open' : '')}>
      {openGameData && <GamePage game={openGameData} index={Math.max(0, games.findIndex(game => game.id === openGameData.id))} settings={settings} running={runningById.get(openGameData.id)} libraryVersion={libraryVersion} actions={pageActions} />}
      {!pageOpen && <>
      <div className="toolbar">
        <div><div className="eyebrow">YOUR COLLECTION</div><h1>{view === 'All Games' ? (store === 'All Games' ? 'All games' : store) : view}</h1></div>
        <div className="toolbar-actions">
          <label className="searchbox"><Search size={16} /><input ref={searchRef} value={query} onChange={event => setQuery(event.target.value)} placeholder="Search your library" /><kbd>Ctrl K</kbd></label>
          <button className="icon-button" title="Scan libraries" onClick={() => void scan()} disabled={scanBusy}><CloudDownload size={17} className={scanBusy ? 'spin' : ''} /></button>
          <button className="secondary-button" onClick={() => void addLocalGame()} disabled={localBusy} title="Pick a PC game (.exe, .lnk, .url) or a ROM file"><Plus size={16} /> Add local game</button>
          <button className="primary-button" onClick={() => setAdding(true)}><Plus size={16} /> Add game</button>
        </div>
      </div>
      <div className="library-meta">
        <span>{filtered.length} GAMES</span><span className="meta-dot" />
        <button onClick={() => setView('Recently Played')}><ListFilter size={13} /> Recently played</button>
        <label className="sort-control">Sort<select value={sortBy} onChange={event => setSortBy(event.target.value as SortBy)}><option value="title">Title</option><option value="lastPlayed">Last played</option><option value="playtime">Playtime</option><option value="library">Library</option></select></label>
        <span className="meta-spacer" />
        <div className="segmented" role="group" aria-label="Installed filter"><button className={onlyInstalled ? 'active' : ''} aria-pressed={onlyInstalled} onClick={() => void setInstalledFilter(true)}>Installed</button><button className={!onlyInstalled ? 'active' : ''} aria-pressed={!onlyInstalled} onClick={() => void setInstalledFilter(false)}>All</button></div>
        <button onClick={() => setView('Favorites')}><Star size={13} /> Favorites</button>
      </div>
      <section className="grid-scroll" ref={scrollRef} onContextMenu={event => { if (!(event.target as HTMLElement).closest('.game-card')) event.preventDefault() }}>
        <div className="virtual-content" style={{ height: virtual.getTotalSize() }}>
          {virtual.getVirtualItems().map(row => {
            const gamesInRow = rows[row.index]
            return <div className="game-row" key={row.key} ref={virtual.measureElement} data-index={row.index} style={{ transform: 'translateY(' + row.start + 'px)', gridTemplateColumns: 'repeat(' + cols + ',minmax(0,1fr))' }}>
              {gamesInRow.map((game, index) => <article
                key={game.id}
                data-game-id={game.id}
                tabIndex={focusedId === game.id ? 0 : -1}
                className="game-card"
                onFocus={() => setFocusedId(game.id)}
                onClick={() => openGame(game)}
                onContextMenu={event => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, game }) }}
              >
                <CardPoster game={game} index={row.index * cols + index}>
                  {runningById.has(game.id) && <span className={'run-badge ' + runningById.get(game.id)!.state}><span className="gp-live-dot" />{runningById.get(game.id)!.state === 'launching' ? 'Launching' : 'Playing'}</span>}
                  {settings.showBadges && <span className="store-badge" title={game.store}><StoreIcon store={game.store} /></span>}
                  <button className="play-overlay" onClick={event => { event.stopPropagation(); void launch(game) }}><Play size={14} fill="currentColor" /> Play</button>
                  <button className="card-more" aria-label="Game actions" onClick={event => { event.stopPropagation(); setMenu({ x: event.clientX, y: event.clientY, game }) }}><MoreHorizontal size={17} /></button>
                </CardPoster>
                <div className="card-title">{game.title}</div>
                <div className="card-subtitle">{game.store}{game.playtimeMinutes > 0 ? ' · ' + Math.floor(game.playtimeMinutes / 60) + 'h played' : ''}</div>
              </article>)}
            </div>
          })}
        </div>
        {!filtered.length && onlyInstalled && !query && games.some(game => (view === 'Hidden' ? game.hidden : !game.hidden) && (store === 'All Games' || game.store === store) && (view !== 'Favorites' || game.favorite) && (view !== 'Recently Played' || !!game.lastPlayed)) ? <div className="empty"><Gamepad2 size={30} /><h3>No installed games here</h3><p>Uninstalled games are hidden. Show all to see games you own but haven't installed.</p><button className="primary-button" onClick={() => void setInstalledFilter(false)}>Show all games</button></div> : !filtered.length && <div className="empty"><Gamepad2 size={30} /><h3>{query ? 'No matching games' : view === 'Hidden' ? 'No hidden games' : 'Your library is empty'}</h3><p>{view === 'Hidden' ? 'Games you hide will appear here.' : 'Scan your stores or add a game manually.'}</p><button className="primary-button" onClick={() => void scan()}><CloudDownload size={15} /> Scan libraries</button></div>}
      </section>
      </>}
    </main>
    {menu && <><div className="menu-dismiss" onMouseDown={() => setMenu(null)} /><div className="context-menu" style={{ left: Math.min(menu.x, window.innerWidth - 230), top: Math.min(menu.y, window.innerHeight - 330) }}>
      {menuOptions.map(option => <button key={option.action} disabled={option.disabled} className={(option.action === 'Remove' ? 'danger ' : '') + (option.disabled ? 'menu-item-disabled' : '')} onClick={() => { if (!option.disabled) void menuAction(option.action, menu.game) }}>{option.label}</button>)}
    </div></>}
    {adding && <div className="modal-scrim"><form className="modal" onSubmit={addGame}>
      <div className="modal-head"><h2>Add a game</h2><button type="button" onClick={() => setAdding(false)}><X size={17} /></button></div>
      <label>Title<input required name="title" placeholder="Game title" autoFocus /></label>
      <label>Store link<input required name="target" placeholder="steam://run/123" /></label>
      <p className="muted-text">For a program, shortcut or ROM file on this PC, use Add local game.</p>
      <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setAdding(false)}>Cancel</button><button className="primary-button">Add game</button></div>
    </form></div>}
    {editingGame && <div className="modal-scrim"><form className="modal edit-modal" onSubmit={saveEdit}>
      <div className="modal-head"><h2>Edit metadata</h2><button type="button" aria-label="Close" onClick={() => setEditingGame(null)}><X size={17} /></button></div>
      <label>Title<input required value={editTitle} onChange={event => setEditTitle(event.target.value)} autoFocus /></label>
      <label>Genre<input value={editGenre} onChange={event => setEditGenre(event.target.value)} placeholder="Optional" /></label>
      <label className="toggle-row"><span>Favorite</span><input type="checkbox" checked={editFavorite} onChange={event => setEditFavorite(event.target.checked)} /></label>
      <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setEditingGame(null)}>Cancel</button><button className="primary-button">Save changes</button></div>
    </form></div>}
    {linkingGame && <div className="modal-scrim"><div className="modal link-modal">
      <div className="modal-head"><h2>Link another game</h2><button type="button" aria-label="Close" onClick={() => setLinkingGame(null)}><X size={17} /></button></div>
      <p>Choose a duplicate to combine into <b>{linkingGame.title}</b>. Its title and metadata stay as the primary entry; launch sources and playtime are combined.</p>
      <label>Find a game<input value={linkQuery} onChange={event => setLinkQuery(event.target.value)} placeholder="Search your library" autoFocus /></label>
      <div className="link-game-list">
        {games.filter(game => game.id !== linkingGame.id && (!linkQuery.trim() || game.title.toLocaleLowerCase().includes(linkQuery.trim().toLocaleLowerCase()))).slice(0, 100).map(game =>
          <button className="link-game-row" key={game.id} onClick={() => void mergeIntoKeptGame(game)}><span><b>{game.title}</b><small>{game.store} · {game.routes.length} launch source{game.routes.length === 1 ? '' : 's'}</small></span><span>Merge</span></button>
        )}
        {games.filter(game => game.id !== linkingGame.id && (!linkQuery.trim() || game.title.toLocaleLowerCase().includes(linkQuery.trim().toLocaleLowerCase()))).length === 0 && <div className="empty-inline">No other matching games.</div>}
      </div>
      <div className="modal-actions"><button className="secondary-button" onClick={() => setLinkingGame(null)}>Cancel</button></div>
    </div></div>}
    {sgdbGame && <div className="modal-scrim"><div className="modal sgdb-modal" role="dialog" aria-label={'Find cover for ' + sgdbGame.title}>
      <div className="modal-head"><h2>{sgdbGame.title}</h2><button type="button" aria-label="Close" onClick={closeFindCover}><X size={17} /></button></div>
      {sgdbPages.state === 'loading' && <div className="empty-inline">Searching SteamGridDB…</div>}
      {sgdbPages.state === 'nokey' && <div className="empty-inline">Add a free SteamGridDB API key in settings to search for covers.<br /><button className="secondary-button" onClick={() => void window.lobby.settings.open('cover-art')}>Open settings</button></div>}
      {sgdbPages.state === 'error' && <div className="empty-inline">{sgdbPages.error}</div>}
      {sgdbPages.state === 'ready' && sgdbCovers.length === 0 && !sgdbPages.hasMore && <div className="empty-inline">No covers found on SteamGridDB for this game</div>}
      {sgdbPages.state === 'ready' && sgdbCovers.length > 0 && <CoverTiles covers={sgdbCovers} animated={settings.steamGridDbAnimated} applying={sgdbApplying} onApply={cover => void applySgdbCover(cover)} />}
      {sgdbPages.state === 'ready' && (sgdbCovers.length > 0 || sgdbPages.hasMore) && <LoadMore shown={sgdbCovers.length} hasMore={sgdbPages.hasMore} loading={sgdbPages.loadingMore} onMore={() => void sgdbPages.loadMore()} noun="cover" />}
      {sgdbPages.state === 'ready' && sgdbPages.error && <div className="empty-inline">{sgdbPages.error}</div>}
      {sgdbApplying && <div className="sgdb-applying" role="status">Saving cover…</div>}
      <div className="modal-actions"><button className="secondary-button" onClick={closeFindCover}>Cancel</button></div>
    </div></div>}
    {editMode && <div className="edit-banner">Edit mode is on · select a title to edit its metadata <button onClick={() => setEditMode(false)}>Done</button></div>}
    {toast && <div className="toast">{toast}</div>}
    {updateNotice && <div className="update-notice" role="status">
      <CloudDownload size={16} />
      <span>Game Lobby {updateNotice.version} is available</span>
      <button type="button" className="primary-button" onClick={() => { setUpdateNotice(null); void window.lobby.settings.open('updates') }}>View update</button>
      <button type="button" className="update-notice-close" title="Dismiss" aria-label="Dismiss update notice" onClick={() => setUpdateNotice(null)}><X size={14} /></button>
    </div>}
  </div>
}

/** A library card's poster: the title on a colored card until the cover has loaded (or when it fails). */
function CardPoster({ game, index, children }: { game: Game; index: number; children: ReactNode }) {
  const { src, status } = useCover(game)
  return <div className={'poster cover-' + status} style={{ ...coverStyle(game, index), aspectRatio: '2 / 3' }}>
    <CoverLayer src={src} status={status} />
    <div className="poster-vignette" />
    <div className="poster-title-mark">{status !== 'loaded' && game.title.toUpperCase()}</div>
    {children}
  </div>
}

function NowPlaying({ game, entry, onOpen }: { game: Game; entry: RunningGame; onOpen: () => void }) {
  const now = useNow(true)
  const launching = entry.state === 'launching'
  return <button className="now-playing" onClick={onOpen} title={'Open ' + game.title}>
    <span className="gp-live-dot" />
    <span className="np-text">{launching ? 'Launching: ' : 'Now playing: '}<b>{game.title}</b>{!launching && ' · ' + formatClock(elapsedSeconds(entry.startedAt, now))}</span>
  </button>
}
