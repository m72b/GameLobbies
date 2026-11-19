import { useEffect, useLayoutEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import Fuse from 'fuse.js'
import { useVirtualizer } from '@tanstack/react-virtual'
import { BarChart3, CloudDownload, EyeOff, Gamepad2, HardDrive, Heart, History, Library, MoreHorizontal, Pencil, Play, Plus, Search, Settings2, Star, X } from 'lucide-react'
import { StoreIcon } from './store-icons'
import { GlMark } from './gl-mark'
import type { CustomLauncher, Game, LauncherTargetKind, RunningGame, Settings, Store as StoreName, SteamGridDbCover, StoreTime, UpdateInfo } from '../shared/types'
import { effectiveLastPlayed, effectiveSeconds } from '../shared/store-time'
import { defaultSettings } from '../shared/defaults'
import { applyTheme } from './theme'
import { coverSrc, coverStyle } from './cover'
import { CoverLayer, useCover } from './poster'
import { CoverTiles, LoadMore, pickerCovers, useSgdbPages } from './cover-picker'
import { StorageView } from './storage-view'
import { StatsView } from './stats-view'
import { RecentShelf } from './recent-shelf'
import { GamePage, type GamePageActions } from './game-page'
import { SettingsView, anchorFor, defaultPage, sectionFor, type SettingsPage } from './settings-view'
import { ApiKeysSheet, KeysUiContext, useApiKeys } from './api-keys'
import type { KeyId } from '../shared/api-keys'
import { applyMotion, currentMotion, durationFor, play } from './motion'
import { beginClose, captureLibrary, fadeIn, originOf, zoomOut, type ZoomOrigin } from './zoom'
import { gridKey, isRectVisible, rectOf, type Rect } from '../shared/motion'
import { LinkReview } from './link-review'
import { routeRef } from '../shared/routes'
import { SessionHistory } from './session-history'
import { elapsedSeconds, formatClock, useNow } from './format'
import { LauncherIcon, useLaunchers } from './launcher-icon'
import { LauncherWaitDialog, LinkLauncherDialog, type WaitingLaunch } from './launcher-assign'
import { displayStore, filterLauncherId, gameLauncher, gameMatchesFilter, identifierProblem, isLabelOnly, kindLabel, launcherFilter, launcherKinds } from '../shared/launchers'

type View = 'All Games' | 'Favorites' | 'Hidden'
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
type MenuAction = 'Play' | 'Edit metadata' | 'Set custom cover' | 'Find cover on SteamGridDB' | 'Hide' | 'Unhide' | 'Add to collection' | 'Link another game' | 'Link to launcher' | 'Open install folder' | 'Remove'

// Saves from the UI leave launch routes alone: program and file routes can only be changed by
// scans and the native pickers, and a stale copy would be refused.
const withoutRoutes = ({ routes: _routes, ...game }: Game): Omit<Game, 'routes'> => game
const isInstalled = (game: Game) => game.routes.some(route => route.installed === true)
const stores: StoreName[] = ['Steam', 'Epic Games', 'GOG', 'EA app', 'Battle.net', 'Xbox', 'Ubisoft', 'Emulator', 'itch.io', 'Amazon Games', 'Custom']

export default function App() {
  const [games, setGames] = useState<Game[]>([])
  // Steam's own playtime per game, computed by the backend (read-only; never written back to games).
  const [storeTimes, setStoreTimes] = useState<Record<string, StoreTime>>({})
  const [settings, setSettings] = useState<Settings>(defaultSettings)
  const [query, setQuery] = useState('')
  const [store, setStore] = useState('All Games')
  const [view, setView] = useState<View>('All Games')
  const [sortBy, setSortByState] = useState<SortBy>(storedSort)
  const setSortBy = (value: SortBy) => { setSortByState(value); rememberSort(value) }
  const [openId, setOpenId] = useState<string | null>(null)
  const [storageOpen, setStorageOpen] = useState(false)
  const [statsOpen, setStatsOpen] = useState(false)
  const [running, setRunning] = useState<RunningGame[]>([])
  const [libraryVersion, setLibraryVersion] = useState(0)
  const savedScroll = useRef(0)
  const [linkingGame, setLinkingGame] = useState<Game | null>(null)
  const [linkQuery, setLinkQuery] = useState('')
  const [linkPicked, setLinkPicked] = useState<string[]>([])
  const [linkReview, setLinkReview] = useState<string[] | null>(null)
  const [menu, setMenu] = useState<{ x: number; y: number; game: Game } | null>(null)
  const [launcherLinkId, setLauncherLinkId] = useState<string | null>(null)
  const [scanBusy, setScanBusy] = useState(false)
  const [localBusy, setLocalBusy] = useState(false)
  const [toast, setToast] = useState('')
  const [updateNotice, setUpdateNotice] = useState<UpdateInfo | null>(null)
  const [adding, setAdding] = useState(false)
  // The add-game dialog's source: '' = a store link, otherwise a custom launcher's id.
  const [addSource, setAddSource] = useState('')
  const [addIdentifier, setAddIdentifier] = useState('')
  // What the game added to a launcher uses, when the launcher allows several kinds.
  const [addKind, setAddKind] = useState<LauncherTargetKind | ''>('')
  const [addBusy, setAddBusy] = useState(false)
  const launchers = useLaunchers()
  const [waiting, setWaiting] = useState<WaitingLaunch | null>(null)
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
  const [historyOpen, setHistoryOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [settingsPage, setSettingsPage] = useState<SettingsPage>(defaultPage)
  const [settingsAnchor, setSettingsAnchor] = useState<string | undefined>()
  const [sheet, setSheet] = useState<{ open: boolean; focus?: KeyId }>({ open: false })
  // Where the game page zooms from, and what it zooms back to; read once, when the page opens or closes.
  const zoomRef = useRef<ZoomOrigin | null>(null)
  const closingRef = useRef<{ id: string; rect: Rect; src?: string } | null>(null)
  const mainRef = useRef<HTMLElement>(null)
  const virtualRef = useRef<HTMLDivElement>(null)
  const openSettingsRef = useRef<(id?: string | null) => void>(() => {})
  const scrollRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const filteredRef = useRef<Game[]>([])
  const focusedIdRef = useRef<string | null>(null)
  const colsRef = useRef(6)
  const scrollToRowRef = useRef<(row: number) => void>(() => {})

  const refresh = async () => {
    const [nextGames, nextSettings, nextTimes] = await Promise.all([window.lobby.library.list(), window.lobby.settings.get(), window.lobby.stats.storeTimes().catch(() => ({}) as Record<string, StoreTime>)])
    setGames(nextGames)
    setStoreTimes(nextTimes)
    setSettings(nextSettings)
  }
  const notice = (message: string) => {
    setToast(message)
    window.setTimeout(() => setToast(''), 2500)
  }
  const launch = async (game: Game, route = 0) => {
    try {
      const outcome = await window.lobby.library.launch(game.id, route)
      if (outcome?.status === 'waiting') {
        // The launcher was started on its own; the game waits for Continue.
        setWaiting({ game, pendingId: outcome.pendingId, launcherName: outcome.launcherName })
        return
      }
      await refresh()
      notice('Launching ' + game.title)
    } catch (error) {
      notice(error instanceof Error ? error.message : 'Could not launch game')
    }
  }
  const keys = useApiKeys(notice)
  /** `from`: the clicked card or shelf item; the page zooms out of its poster. */
  const openGame = (game: Game, from?: Element | null) => {
    if (!settingsOpen && !openId) savedScroll.current = scrollRef.current?.scrollTop ?? savedScroll.current
    zoomRef.current = originOf(from, coverSrc(game.cover))
    if (zoomRef.current && !openId && !settingsOpen && !storageOpen && !statsOpen) captureLibrary(mainRef.current)
    setSettingsOpen(false)
    setStatsOpen(false)
    setOpenId(game.id)
    setMenu(null)
  }
  const closeGame = () => {
    const current = openId ? games.find(game => game.id === openId) : undefined
    const hero = document.querySelector('.gp-hero')
    const heroImage = document.querySelector<HTMLImageElement>('.gp-hero-img.ready')
    closingRef.current = current && hero ? { id: current.id, rect: rectOf(hero.getBoundingClientRect()), src: heroImage?.currentSrc || coverSrc(current.cover) } : null
    beginClose(document.querySelector<HTMLElement>('.game-page'))
    zoomRef.current = null
    setOpenId(null)
  }
  /** Shows Settings in place of the library, optionally on the page for a `settings.open` section id. */
  const openSettings = (id?: string | null) => {
    const page = sectionFor(id ?? null)
    if (!settingsOpen && !openId) savedScroll.current = scrollRef.current?.scrollTop ?? savedScroll.current
    setOpenId(null)
    setStorageOpen(false)
    setStatsOpen(false)
    if (page) { setSettingsPage(page); setSettingsAnchor(anchorFor(id ?? null)) }
    setSettingsOpen(true)
  }
  openSettingsRef.current = openSettings
  const closeSettings = () => setSettingsOpen(false)
  /** Shows Statistics in place of the library (like Settings; Back or Escape returns). */
  const openStats = () => {
    if (!settingsOpen && !openId && !statsOpen) savedScroll.current = scrollRef.current?.scrollTop ?? savedScroll.current
    setOpenId(null)
    setSettingsOpen(false)
    setStorageOpen(false)
    setStatsOpen(true)
  }
  const closeStats = () => setStatsOpen(false)
  const openSheet = (focus?: KeyId) => setSheet({ open: true, focus })
  const closeSheet = () => setSheet(current => ({ ...current, open: false }))
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
    setLinkPicked([])
    setMenu(null)
  }
  // Linking always goes through the review step (`LinkReview`): nothing merges from this list.
  const toggleLinkPick = (id: string) => setLinkPicked(current => current.includes(id) ? current.filter(other => other !== id) : [...current, id])
  const linked = (nextGames: Game[], keptId: string) => {
    setGames(nextGames)
    setLinkReview(null)
    setLinkingGame(null)
    if (openId && !nextGames.some(game => game.id === openId)) setOpenId(keptId)
    notice('Games linked')
  }
  const menuAction = async (action: MenuAction, game: Game) => {
    setMenu(null)
    if (action === 'Play') return launch(game)
    if (action === 'Edit metadata') return beginEdit(game)
    if (action === 'Set custom cover') return setCover(game)
    if (action === 'Find cover on SteamGridDB') return findCover(game)
    if (action === 'Link another game') return linkAnotherGame(game)
    if (action === 'Link to launcher') return setLauncherLinkId(game.id)
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
  const openAdd = (source = '') => {
    setAddSource(source)
    setAddIdentifier('')
    setAddKind('')
    setAdding(true)
  }
  /** The kind a game added to `launcher` uses: the chosen one, else the launcher's first. */
  const kindFor = (launcher: CustomLauncher): LauncherTargetKind => {
    const kinds = launcherKinds(launcher)
    return addKind && kinds.includes(addKind) ? addKind : kinds[0]
  }
  const addGame = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const data = new FormData(event.currentTarget)
    const launcher = launchers.find(item => item.id === addSource)
    if (launcher) {
      const kind = kindFor(launcher)
      const typed = kind === 'identifier' && !isLabelOnly(launcher)
      if (typed && identifierProblem(addIdentifier)) return
      setAddBusy(true)
      try {
        const result = await window.lobby.launchers.addGame(launcher.id, String(data.get('title') ?? ''), typed ? addIdentifier : null, isLabelOnly(launcher) ? undefined : kind)
        if (!result) return
        setGames(result.games)
        setAdding(false)
        const added = result.games.find(game => game.id === result.gameId)
        if (added) openGame(added)
        notice(added ? 'Added ' + added.title + ' to ' + launcher.name : 'Game added')
      } catch (error) {
        notice(error instanceof Error ? error.message : 'Could not add the game')
      } finally {
        setAddBusy(false)
      }
      return
    }
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
    const offOpen = window.lobby.settings.onOpen(id => openSettingsRef.current(id))
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
      if ((event.ctrlKey || event.metaKey) && event.key === ',') {
        event.preventDefault()
        openSettings()
        return
      }
      if (event.key === 'Escape') {
        if (sheet.open) return
        const dialogOpen = !!(menu || adding || editingGame || sgdbGame || linkingGame || linkReview || launcherLinkId || document.querySelector('.modal-scrim'))
        if (settingsOpen && !dialogOpen) {
          // A focused field gives up focus first; a second Escape leaves Settings.
          if (typing) target.blur(); else closeSettings()
          return
        }
        if (statsOpen && !dialogOpen && !openId) {
          if (typing) target.blur(); else closeStats()
          return
        }
        if (!dialogOpen && openId) closeGame()
        setMenu(null)
        setLauncherLinkId(null)
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
          openGame(game, scrollRef.current?.querySelector('[data-game-id="' + CSS.escape(game.id) + '"]'))
        }
      }
    }
    window.addEventListener('keydown', key)
    return () => {
      offLibrary()
      offTracking()
      offSettings()
      offOpen()
      offSgdb()
      observer.disconnect()
      window.removeEventListener('keydown', key)
    }
  }, [openId, menu, adding, editingGame, sgdbGame, linkingGame, linkReview, launcherLinkId, settingsOpen, statsOpen, sheet.open])

  useEffect(() => { applyTheme(settings.theme, settings); return applyMotion(settings.motion, settings.animations) }, [settings])
  useEffect(() => {
    // Startup update check (the backend skips it when turned off or done in the last 24 h).
    const timer = window.setTimeout(() => {
      void window.lobby.updates.autoCheck().then(info => { if (info?.available) setUpdateNotice(info) }).catch(() => undefined)
    }, 10_000)
    return () => window.clearTimeout(timer)
  }, [])
  const openGameData = openId ? games.find(game => game.id === openId) : undefined
  const pageOpen = !!openGameData && !settingsOpen
  // The library is out of view while a game page or Settings is showing.
  const awayFromLibrary = pageOpen || settingsOpen || statsOpen
  const wasAway = useRef(false)
  useLayoutEffect(() => {
    if (wasAway.current && !awayFromLibrary && scrollRef.current) {
      const top = savedScroll.current
      scrollRef.current.scrollTop = top
      requestAnimationFrame(() => { if (scrollRef.current) scrollRef.current.scrollTop = top })
      // Closing a game page: its art shrinks back onto the poster when that is still on screen.
      const closing = closingRef.current
      closingRef.current = null
      if (closing) {
        const find = () => {
          const poster = scrollRef.current?.querySelector('[data-game-id="' + CSS.escape(closing.id) + '"] .poster')
          const grid = scrollRef.current?.getBoundingClientRect()
          const box = poster?.getBoundingClientRect()
          if (!poster || !grid || !box || !isRectVisible(rectOf(box), rectOf(grid))) return null
          return { rect: rectOf(box), radius: Number.parseFloat(getComputedStyle(poster).borderTopLeftRadius) || 10 }
        }
        const parts = mainRef.current ? Array.from(mainRef.current.children) : []
        requestAnimationFrame(() => requestAnimationFrame(() => {
          const grid = scrollRef.current?.getBoundingClientRect()
          zoomOut(closing.rect, closing.src, find(), parts, grid ? { x: grid.left + grid.width / 2, y: grid.top + grid.height / 2 } : undefined)
        }))
      } else if (mainRef.current && !pageOpen) {
        fadeIn(Array.from(mainRef.current.children), 160)
      }
    }
    wasAway.current = awayFromLibrary
  }, [awayFromLibrary, pageOpen])
  useEffect(() => {
    const resize = () => setGridWidth(scrollRef.current?.clientWidth || window.innerWidth - 90)
    if (!scrollRef.current) return
    const observer = new ResizeObserver(resize)
    observer.observe(scrollRef.current)
    resize()
    return () => observer.disconnect()
  }, [awayFromLibrary])
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
      gameMatchesFilter(game, store) &&
      (view !== 'Favorites' || game.favorite)
    )
  }, [games, query, store, view, fuzzy, onlyInstalled])
  const railStores = useMemo(() => stores.filter(item => item === store || games.some(game => game.store === item && !game.hidden && (!onlyInstalled || isInstalled(game)))).slice(0, 8), [games, store, onlyInstalled])
  const activeLauncher = useMemo(() => { const id = filterLauncherId(store); return id ? launchers.find(item => item.id === id) : undefined }, [store, launchers])
  // A launcher filter whose launcher was deleted goes back to all games.
  useEffect(() => { if (filterLauncherId(store) && !activeLauncher) setStore('All Games') }, [store, activeLauncher])
  const storeName = (game: Game) => displayStore(game, launchers)
  const ordered = useMemo(() => {
    if (query.trim()) return filtered
    const name = (game: Game) => displayStore(game, launchers)
    return [...filtered].sort((a, b) =>
      sortBy === 'lastPlayed' ? effectiveLastPlayed(b, storeTimes) - effectiveLastPlayed(a, storeTimes) :
        sortBy === 'playtime' ? effectiveSeconds(b, storeTimes) - effectiveSeconds(a, storeTimes) :
          sortBy === 'library' ? name(a).localeCompare(name(b)) || a.title.localeCompare(b.title) : a.title.localeCompare(b.title)
    )
  }, [filtered, query, sortBy, launchers, storeTimes])
  // Launcher, store, view or Installed/All changes: the old grid fades out, then the new games fade in.
  // Library updates and typing in search do not animate.
  const filterKey = gridKey(view, store, onlyInstalled)
  const [shownKey, setShownKey] = useState(filterKey)
  const shownGames = useRef<Game[]>([])
  const swapping = shownKey !== filterKey
  if (!swapping) shownGames.current = ordered
  const gridGames = swapping ? shownGames.current : ordered
  const cols = Math.max(2, Math.floor(gridWidth / (settings.posterWidth + Number.parseInt(settings.theme.gap || '9'))))
  filteredRef.current = ordered
  focusedIdRef.current = focusedId
  colsRef.current = cols
  const rows = useMemo(() => Array.from({ length: Math.ceil(gridGames.length / cols) }, (_, index) => gridGames.slice(index * cols, (index + 1) * cols)), [gridGames, cols])
  const virtual = useVirtualizer({ count: rows.length, getScrollElement: () => scrollRef.current, estimateSize: () => settings.posterWidth * 1.5 + 62, overscan: 3 })
  scrollToRowRef.current = row => virtual.scrollToIndex(row, { align: 'auto' })
  useEffect(() => {
    if (shownKey === filterKey) return
    const target = virtualRef.current
    const ms = durationFor(140, currentMotion())
    const swap = () => { if (scrollRef.current) scrollRef.current.scrollTop = 0; setShownKey(filterKey) }
    const fade = target && ms > 0 ? play(target, [{ opacity: 1 }, { opacity: 0 }], ms, { fill: 'forwards' }) : null
    if (!fade) { swap(); return }
    let live = true
    fade.finished.then(() => { if (live) swap() }, () => { if (live) swap() })
    return () => { live = false }
  }, [filterKey, shownKey])
  const lastShown = useRef(shownKey)
  useLayoutEffect(() => {
    if (lastShown.current === shownKey) return
    lastShown.current = shownKey
    const target = virtualRef.current
    if (!target) return
    target.getAnimations().forEach(animation => animation.cancel())
    const level = currentMotion()
    play(target, level === 'full' ? [{ opacity: 0, transform: 'translateY(6px)' }, { opacity: 1, transform: 'none' }] : [{ opacity: 0 }, { opacity: 1 }], durationFor(200, level), { fill: 'backwards' })
  }, [shownKey])
  const visibleRowsKey = virtual.getVirtualItems().map(row => row.index).join(',')
  useEffect(() => {
    if (!focusedId) return
    const escaped = CSS.escape(focusedId)
    scrollRef.current?.querySelector<HTMLElement>('[data-game-id="' + escaped + '"]')?.focus({ preventScroll: true })
  }, [focusedId, visibleRowsKey])
  useEffect(() => {
    if (focusedId && !ordered.some(game => game.id === focusedId)) setFocusedId(null)
  }, [ordered, focusedId])

  const launcherLinkGame = launcherLinkId ? games.find(game => game.id === launcherLinkId) : undefined
  const menuOptions: Array<{ action: MenuAction; label: string; disabled?: boolean }> = [
    { action: 'Play', label: 'Play' },
    { action: 'Edit metadata', label: 'Edit metadata' },
    { action: 'Set custom cover', label: 'Set custom cover' },
    { action: 'Find cover on SteamGridDB', label: 'Find cover on SteamGridDB' },
    { action: 'Link another game', label: 'Link another game' },
    ...(launchers.length > 0 ? [{ action: 'Link to launcher' as const, label: 'Link to launcher…' }] : []),
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
    setEmulator: async (game, routeIndex, emulatorId) => { setGames(await window.lobby.library.setEmulator(game.id, routeIndex, emulatorId)) },
    linkLauncher: game => setLauncherLinkId(game.id),
    unassignLauncher: async (game, launcherId) => { setGames(await window.lobby.launchers.unassign(game.id, launcherId)) },
    renameRoute: async (game, index, name) => { setGames(await window.lobby.library.renameRoute(game.id, routeRef(game.routes[index], index), name)) },
    setRouteArgs: async (game, index, args) => { setGames(await window.lobby.library.setRouteArgs(game.id, routeRef(game.routes[index], index), args)) },
    routeArgsPending: (game, index) => window.lobby.library.routeArgsPending(game.id, routeRef(game.routes[index], index)),
    routeArgsCheck: (game, index, args) => window.lobby.library.routeArgsCheck(game.id, routeRef(game.routes[index], index), args),
    unlinkPreview: (game, index) => window.lobby.library.unlinkPreview(game.id, routeRef(game.routes[index], index)),
    unlink: async (game, index) => {
      const result = await window.lobby.library.unlink(game.id, routeRef(game.routes[index], index))
      setGames(result.games)
      const split = result.games.find(other => other.id === result.gameId)
      notice(split ? 'Unlinked: ' + split.title + ' is its own game again' : 'Unlinked')
    }
  }

  return <KeysUiContext.Provider value={{ keys, openSheet, openTab: () => openSettings('api-keys') }}><div className="app-shell">
    <header className="titlebar">
      <div className="brand"><GlMark size={12} className="gl-mark" />Game Lobby</div>{nowPlaying && !(pageOpen && openId === nowPlaying.game.id) && <NowPlaying game={nowPlaying.game} entry={nowPlaying.item} onOpen={() => openGame(nowPlaying.game)} />}<div className="drag-space" data-tauri-drag-region />
      <button className="title-action" onClick={() => window.lobby.window.minimize()} aria-label="Minimize">—</button>
      <button className="title-action" onClick={() => window.lobby.window.toggleMaximize()} aria-label="Maximize">□</button>
      <button className="title-action close" onClick={() => window.lobby.window.close()} aria-label="Close">×</button>
    </header>
    <aside className={'rail ' + (collapsed ? 'compact' : '')}>
      <button className="rail-button" onClick={() => setCollapsed(!collapsed)} title="Menu"><GlMark size={16} className="gl-mark" /></button>
      <button className="rail-button search-button" onClick={() => searchRef.current?.focus()} title="Search"><Search /></button>
      <div className="rail-sep" />
      <button className={'rail-button ' + (view === 'All Games' ? 'active' : '')} onClick={() => { setStorageOpen(false); setSettingsOpen(false); setStatsOpen(false); setView('All Games'); setStore('All Games') }} title="All games"><Library /></button>
      <button className={'rail-button ' + (view === 'Favorites' ? 'active' : '')} onClick={() => { setStorageOpen(false); setSettingsOpen(false); setStatsOpen(false); setView('Favorites') }} title="Favorites"><Heart /></button>
      <button className={'rail-button ' + (statsOpen ? 'active' : '')} onClick={() => statsOpen ? closeStats() : openStats()} title="Statistics"><BarChart3 /></button>
      <button className={'rail-button ' + (view === 'Hidden' ? 'active' : '')} onClick={() => { setStorageOpen(false); setSettingsOpen(false); setStatsOpen(false); setView('Hidden') }} title="Hidden games"><EyeOff /></button>
      <button className={'rail-button ' + (storageOpen ? 'active' : '')} onClick={() => { setOpenId(null); setSettingsOpen(false); setStatsOpen(false); setStorageOpen(!storageOpen) }} title="Storage"><HardDrive /></button>
      <button className={'rail-button ' + (historyOpen ? 'active' : '')} onClick={() => setHistoryOpen(true)} title="Play history (all games)"><History /></button>
      <div className="rail-sep" />
      <div className="rail-stores">{railStores.map(item =>
        <button key={item} className={'rail-button mini ' + (store === item ? 'active' : '')} title={item} onClick={() => { setSettingsOpen(false); setStatsOpen(false); setStorageOpen(false); setStore(store === item ? 'All Games' : item); setView('All Games') }}><StoreIcon store={item} size={16} /></button>
      )}{launchers.map(item => {
        const filter = launcherFilter(item.id)
        return <button key={filter} className={'rail-button mini ' + (store === filter ? 'active' : '')} title={item.name} aria-label={item.name} onClick={() => { setSettingsOpen(false); setStatsOpen(false); setStorageOpen(false); setStore(store === filter ? 'All Games' : filter); setView('All Games') }}><LauncherIcon launcher={item} size={16} /></button>
      })}<button className="rail-button mini" title="Add source" onClick={() => openSettings()}><Plus /></button></div>
      <div className="rail-bottom">
        <button className="rail-button" title="Export library" onClick={() => void window.lobby.backup.export()}><CloudDownload /></button>
        <button className={'rail-button ' + (editMode ? 'active' : '')} title="Edit mode" onClick={() => setEditMode(!editMode)}><Pencil /></button>
        <button className={'rail-button ' + (settingsOpen ? 'active' : '')} title="Settings (Ctrl+,)" onClick={() => openSettings()}><Settings2 /></button>
        <button className="rail-button" title="Quit" onClick={() => window.lobby.app.quit()}><X /></button>
      </div>
    </aside>
    <main ref={mainRef} className={'main' + (pageOpen ? ' page-open' : '') + (settingsOpen ? ' settings-open' : '') + (statsOpen && !pageOpen && !settingsOpen ? ' stats-open' : '')}>
      {settingsOpen && <SettingsView settings={settings} onSettings={setSettings} say={notice} keys={keys} page={settingsPage} onPage={setSettingsPage} anchor={settingsAnchor} onAnchorDone={() => setSettingsAnchor(undefined)} onClose={closeSettings} />}
      {pageOpen && openGameData && <GamePage origin={zoomRef.current} game={openGameData} index={Math.max(0, games.findIndex(game => game.id === openGameData.id))} settings={settings} launchers={launchers} running={runningById.get(openGameData.id)} libraryVersion={libraryVersion} actions={pageActions} />}
      {!pageOpen && !settingsOpen && statsOpen && <StatsView say={notice} onClose={closeStats} onOpenGame={id => { const game = games.find(item => item.id === id); if (game) openGame(game) }} />}
      {!pageOpen && !settingsOpen && !statsOpen && storageOpen && <StorageView notice={notice} onClose={() => setStorageOpen(false)} onOpen={id => { const game = games.find(item => item.id === id); if (game) { setStorageOpen(false); openGame(game) } }} />}
      {!pageOpen && !settingsOpen && !statsOpen && !storageOpen && <>
      <div className="toolbar">
        <div><div className="eyebrow">YOUR COLLECTION</div><h1>{view === 'All Games' ? (store === 'All Games' ? 'All games' : activeLauncher ? activeLauncher.name : store) : view}</h1></div>
        <div className="toolbar-actions">
          <label className="searchbox"><Search size={16} /><input ref={searchRef} value={query} onChange={event => setQuery(event.target.value)} placeholder="Search your library" /><kbd>Ctrl K</kbd></label>
          <button className="icon-button" title="Scan libraries" onClick={() => void scan()} disabled={scanBusy}><CloudDownload size={17} className={scanBusy ? 'spin' : ''} /></button>
          <button className="secondary-button" onClick={() => void addLocalGame()} disabled={localBusy} title="Pick a PC game (.exe, .lnk, .url) or a ROM file"><Plus size={16} /> Add local game</button>
          {activeLauncher
            ? <button className="primary-button" onClick={() => openAdd(activeLauncher.id)}><Plus size={16} /> Add game to {activeLauncher.name}</button>
            : <button className="primary-button" onClick={() => openAdd()}><Plus size={16} /> Add game</button>}
        </div>
      </div>
      <div className="library-meta">
        <span>{filtered.length} GAMES</span><span className="meta-dot" />
        <button onClick={openStats}><BarChart3 size={13} /> Statistics</button>
        <button onClick={() => setHistoryOpen(true)}><History size={13} /> Play history</button>
        <label className="sort-control">Sort<select value={sortBy} onChange={event => setSortBy(event.target.value as SortBy)}><option value="title">Title</option><option value="lastPlayed">Last played</option><option value="playtime">Playtime</option><option value="library">Library</option></select></label>
        <span className="meta-spacer" />
        <div className="segmented" role="group" aria-label="Installed filter"><button className={onlyInstalled ? 'active' : ''} aria-pressed={onlyInstalled} onClick={() => void setInstalledFilter(true)}>Installed</button><button className={!onlyInstalled ? 'active' : ''} aria-pressed={!onlyInstalled} onClick={() => void setInstalledFilter(false)}>All</button></div>
        <button onClick={() => setView('Favorites')}><Star size={13} /> Favorites</button>
      </div>
      {view === 'All Games' && store === 'All Games' && !query && <RecentShelf games={games} libraryVersion={libraryVersion} onOpen={openGame} />}
      <section className="grid-scroll" ref={scrollRef} onContextMenu={event => { if (!(event.target as HTMLElement).closest('.game-card')) event.preventDefault() }}>
        <div className="virtual-content" ref={virtualRef} style={{ height: virtual.getTotalSize() }}>
          {virtual.getVirtualItems().map(row => {
            const gamesInRow = rows[row.index]
            return <div className="game-row" key={row.key} ref={virtual.measureElement} data-index={row.index} style={{ transform: 'translateY(' + row.start + 'px)', gridTemplateColumns: 'repeat(' + cols + ',minmax(0,1fr))' }}>
              {gamesInRow.map((game, index) => <article
                key={game.id}
                data-game-id={game.id}
                tabIndex={focusedId === game.id ? 0 : -1}
                className="game-card"
                onFocus={() => setFocusedId(game.id)}
                onClick={event => openGame(game, event.currentTarget)}
                onContextMenu={event => { event.preventDefault(); setMenu({ x: event.clientX, y: event.clientY, game }) }}
              >
                <CardPoster game={game} index={row.index * cols + index}>
                  {runningById.has(game.id) && <span className={'run-badge ' + runningById.get(game.id)!.state}><span className="gp-live-dot" />{runningById.get(game.id)!.state === 'launching' ? 'Launching' : 'Playing'}</span>}
                  {settings.showBadges && <CardBadge game={game} launchers={launchers} />}
                  <button className="play-overlay" onClick={event => { event.stopPropagation(); void launch(game) }}><Play size={14} fill="currentColor" /> Play</button>
                  <button className="card-more" aria-label="Game actions" onClick={event => { event.stopPropagation(); setMenu({ x: event.clientX, y: event.clientY, game }) }}><MoreHorizontal size={17} /></button>
                </CardPoster>
                <div className="card-title">{game.title}</div>
                <div className="card-subtitle">{storeName(game)}{effectiveSeconds(game, storeTimes) >= 60 ? ' · ' + Math.floor(effectiveSeconds(game, storeTimes) / 3600) + 'h played' : ''}</div>
              </article>)}
            </div>
          })}
        </div>
        {!swapping && !filtered.length && onlyInstalled && !query && games.some(game => (view === 'Hidden' ? game.hidden : !game.hidden) && gameMatchesFilter(game, store) && (view !== 'Favorites' || game.favorite)) ? <div className="empty"><Gamepad2 size={30} /><h3>No installed games here</h3><p>Uninstalled games are hidden. Show all to see games you own but haven't installed.</p><button className="primary-button" onClick={() => void setInstalledFilter(false)}>Show all games</button></div> : !swapping && !filtered.length && <div className="empty"><Gamepad2 size={30} /><h3>{query ? 'No matching games' : view === 'Hidden' ? 'No hidden games' : 'Your library is empty'}</h3><p>{view === 'Hidden' ? 'Games you hide will appear here.' : 'Scan your stores or add a game manually.'}</p><button className="primary-button" onClick={() => void scan()}><CloudDownload size={15} /> Scan libraries</button></div>}
      </section>
      </>}
    </main>
    {menu && <><div className="menu-dismiss" onMouseDown={() => setMenu(null)} /><div className="context-menu" style={{ left: Math.min(menu.x, window.innerWidth - 230), top: Math.min(menu.y, window.innerHeight - 365) }}>
      {menuOptions.map(option => <button key={option.action} disabled={option.disabled} className={(option.action === 'Remove' ? 'danger ' : '') + (option.disabled ? 'menu-item-disabled' : '')} onClick={() => { if (!option.disabled) void menuAction(option.action, menu.game) }}>{option.label}</button>)}
    </div></>}
    {historyOpen && <SessionHistory running={running} onClose={() => setHistoryOpen(false)} />}
    {adding && <div className="modal-scrim"><form className="modal" onSubmit={addGame}>
      {(() => {
        const launcher = launchers.find(item => item.id === addSource)
        const labelOnly = !!launcher && isLabelOnly(launcher)
        const kinds = launcher && !labelOnly ? launcherKinds(launcher) : []
        const kind = launcher ? (labelOnly ? 'file' : kindFor(launcher)) : undefined
        const identifierError = kind === 'identifier' && addIdentifier ? identifierProblem(addIdentifier) : ''
        return <>
          <div className="modal-head"><h2>{launcher ? 'Add game to ' + launcher.name : 'Add a game'}</h2><button type="button" aria-label="Close" onClick={() => setAdding(false)}><X size={17} /></button></div>
          {launchers.length > 0 && <label>Launch with<select value={addSource} onChange={event => { setAddSource(event.target.value); setAddIdentifier('') }}>
            <option value="">A store link</option>
            {launchers.map(item => <option key={item.id} value={item.id}>{item.name}</option>)}
          </select></label>}
          {kinds.length > 1 && <div className="field-label">This game uses
            <div className="chip-group" role="radiogroup" aria-label="What this game uses">
              {kinds.map(item => <button key={item} type="button" role="radio" aria-checked={kind === item} className={'chip ' + (kind === item ? 'active' : '')} onClick={() => { setAddKind(item); setAddIdentifier('') }}>{kindLabel(item)}</button>)}
            </div></div>}
          <label>Title<input required={!launcher || kind === 'identifier'} name="title" placeholder={launcher && kind !== 'identifier' ? 'Optional: the ' + kind + ' name is used' : 'Game title'} autoFocus /></label>
          {!launcher && <>
            <label>Store link<input required name="target" placeholder="steam://run/123" /></label>
            <p className="muted-text">For a program, shortcut or ROM file on this PC, use Add local game.</p>
          </>}
          {launcher && kind === 'identifier' && <>
            <label>Identifier<input required value={addIdentifier} onChange={event => setAddIdentifier(event.target.value)} placeholder="The game’s id in this launcher" spellCheck={false} aria-invalid={!!identifierError} /></label>
            {identifierError ? <div className="form-error" role="alert">{identifierError}</div> : <p className="muted-text">Passed to {launcher.name} as one argument, exactly as typed. Up to 200 letters, digits, spaces and . _ : @ # ( ) ' + , ! - characters; it can’t start with “-” or “@”, or like a drive (“C:”).</p>}
          </>}
          {launcher && labelOnly && <p className="muted-text">Choose the game’s file next: a program (.exe), a shortcut or a ROM. It runs directly, the same way as Add local game, and shows {launcher.name} as where it is played from. To add many games at once, give {launcher.name} a games folder in Settings → Custom launchers.</p>}
          {launcher && !labelOnly && kind !== 'identifier' && <p className="muted-text">Choose the game’s {kind} next. It is passed to {launcher.name} as one argument.</p>}
          <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setAdding(false)}>Cancel</button><button className="primary-button" disabled={addBusy || !!identifierError}>{launcher && kind !== 'identifier' ? 'Choose ' + kind + '…' : 'Add game'}</button></div>
        </>
      })()}
    </form></div>}
    {editingGame && <div className="modal-scrim"><form className="modal edit-modal" onSubmit={saveEdit}>
      <div className="modal-head"><h2>Edit metadata</h2><button type="button" aria-label="Close" onClick={() => setEditingGame(null)}><X size={17} /></button></div>
      <label>Title<input required value={editTitle} onChange={event => setEditTitle(event.target.value)} autoFocus /></label>
      <label>Genre<input value={editGenre} onChange={event => setEditGenre(event.target.value)} placeholder="Optional" /></label>
      <label className="toggle-row"><span>Favorite</span><input type="checkbox" checked={editFavorite} onChange={event => setEditFavorite(event.target.checked)} /></label>
      <div className="modal-actions"><button type="button" className="secondary-button" onClick={() => setEditingGame(null)}>Cancel</button><button className="primary-button">Save changes</button></div>
    </form></div>}
    {linkingGame && !linkReview && <div className="modal-scrim"><div className="modal link-modal">
      <div className="modal-head"><h2>Link games</h2><button type="button" aria-label="Close" onClick={() => setLinkingGame(null)}><X size={17} /></button></div>
      <p>Choose the other entries of <b>{linkingGame.title}</b> (other stores, editions or versions, even from the same store). Next you review what is combined and pick the title, cover and other details. Each entry becomes a route under “Play from”.</p>
      <label>Find a game<input value={linkQuery} onChange={event => setLinkQuery(event.target.value)} placeholder="Search your library" autoFocus /></label>
      <div className="link-game-list">
        {games.filter(game => game.id !== linkingGame.id && (!linkQuery.trim() || game.title.toLocaleLowerCase().includes(linkQuery.trim().toLocaleLowerCase()))).slice(0, 100).map(game =>
          <button className={'link-game-row' + (linkPicked.includes(game.id) ? ' picked' : '')} key={game.id} aria-pressed={linkPicked.includes(game.id)} onClick={() => toggleLinkPick(game.id)}><span><b>{game.title}</b><small>{storeName(game)} · {game.routes.length} launch source{game.routes.length === 1 ? '' : 's'}</small></span><span>{linkPicked.includes(game.id) ? 'Selected' : 'Select'}</span></button>
        )}
        {games.filter(game => game.id !== linkingGame.id && (!linkQuery.trim() || game.title.toLocaleLowerCase().includes(linkQuery.trim().toLocaleLowerCase()))).length === 0 && <div className="empty-inline">No other matching games.</div>}
      </div>
      <div className="modal-actions"><button className="secondary-button" onClick={() => setLinkingGame(null)}>Cancel</button><button className="primary-button" disabled={linkPicked.length === 0} onClick={() => setLinkReview([linkingGame.id, ...linkPicked])}>Review link{linkPicked.length > 1 ? ' (' + (linkPicked.length + 1) + ' games)' : ''}…</button></div>
    </div></div>}
    {launcherLinkGame && <LinkLauncherDialog game={launcherLinkGame} launchers={launchers} onGames={setGames} onClose={() => setLauncherLinkId(null)} />}
    {linkReview && <LinkReview gameIds={linkReview} onClose={() => setLinkReview(null)} onLinked={linked} />}
    {sgdbGame && <div className="modal-scrim"><div className="modal sgdb-modal" role="dialog" aria-label={'Find cover for ' + sgdbGame.title}>
      <div className="modal-head"><h2>{sgdbGame.title}</h2><button type="button" aria-label="Close" onClick={closeFindCover}><X size={17} /></button></div>
      {sgdbPages.state === 'loading' && <div className="empty-inline">Searching SteamGridDB…</div>}
      {sgdbPages.state === 'nokey' && <div className="empty-inline">Add a free SteamGridDB API key to search for covers.<br /><button className="secondary-button" onClick={() => openSheet('steamgriddb')}>Add API key</button></div>}
      {sgdbPages.state === 'error' && <div className="empty-inline">{sgdbPages.error}</div>}
      {sgdbPages.state === 'ready' && sgdbCovers.length === 0 && !sgdbPages.hasMore && <div className="empty-inline">No covers found on SteamGridDB for this game</div>}
      {sgdbPages.state === 'ready' && sgdbCovers.length > 0 && <CoverTiles covers={sgdbCovers} animated={settings.steamGridDbAnimated} applying={sgdbApplying} onApply={cover => void applySgdbCover(cover)} />}
      {sgdbPages.state === 'ready' && (sgdbCovers.length > 0 || sgdbPages.hasMore) && <LoadMore shown={sgdbCovers.length} hasMore={sgdbPages.hasMore} loading={sgdbPages.loadingMore} onMore={() => void sgdbPages.loadMore()} noun="cover" />}
      {sgdbPages.state === 'ready' && sgdbPages.error && <div className="empty-inline">{sgdbPages.error}</div>}
      {sgdbApplying && <div className="sgdb-applying" role="status">Saving cover…</div>}
      <div className="modal-actions"><button className="secondary-button" onClick={closeFindCover}>Cancel</button></div>
    </div></div>}
    {editMode && <div className="edit-banner">Edit mode is on · select a title to edit its metadata <button onClick={() => setEditMode(false)}>Done</button></div>}
    {waiting && <LauncherWaitDialog key={waiting.pendingId} wait={waiting} onClose={() => setWaiting(null)} onStarted={() => { const game = waiting.game; setWaiting(null); notice('Launching ' + game.title); void refresh() }} />}
    {toast && <div className="toast">{toast}</div>}
    {updateNotice && <div className="update-notice" role="status">
      <CloudDownload size={16} />
      <span>Game Lobby {updateNotice.version} is available</span>
      <button type="button" className="primary-button" onClick={() => { setUpdateNotice(null); openSettings('updates') }}>View update</button>
      <button type="button" className="update-notice-close" title="Dismiss" aria-label="Dismiss update notice" onClick={() => setUpdateNotice(null)}><X size={14} /></button>
    </div>}
    <ApiKeysSheet open={sheet.open} focus={sheet.focus} keys={keys} onClose={closeSheet} />
  </div></KeysUiContext.Provider>
}

/** A card's store badge: the launcher's icon for Custom games run by a custom launcher. */
function CardBadge({ game, launchers }: { game: Game; launchers: CustomLauncher[] }) {
  const launcher = game.store === 'Custom' ? gameLauncher(game, launchers) : undefined
  return <span className="store-badge" title={launcher ? launcher.name : game.store}>{launcher ? <LauncherIcon launcher={launcher} /> : <StoreIcon store={game.store} />}</span>
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
