import { useCallback, useEffect, useMemo, useState } from 'react'
import { ArrowLeft, ChevronDown, FolderOpen, Heart, MoreHorizontal, Pencil, Play } from 'lucide-react'
import { StoreIcon } from './store-icons'
import { CustomizePanel, useLogoLayout } from './customize-page'
import { coverUrl } from './lobby-api'
import { defaultGamePageCustom } from '../shared/defaults'
import { coverLayerStyle, coverStyle } from './cover'
import { CoverLayer, useCover } from './poster'
import { GenreBlock, IgdbAbout, IgdbPicker, IgdbRatings, useIgdb } from './igdb-details'
import { isEmulatorRoute, routeLabel, routeTitle } from '../shared/routes'
import { ImageWall, useScreenshots } from './image-wall'
import { GameNotesSection, YourScore } from './game-notes'
import { ControllerBadge, ControllerBlock, useGameInput } from './controller-info'
import { elapsedSeconds, formatClock, formatDate, formatSession, formatTime, formatTotal, useNow } from './format'
import type { EmulatorCatalog, EmulatorProfile, Game, GamePageArt, GameRoute, PlaySession, PlayStats, RunningGame, Settings } from '../shared/types'

export interface GamePageActions {
  back(): void
  play(game: Game, route: number): void
  edit(game: Game): void
  setCover(game: Game): void
  findCover(game: Game): void
  link(game: Game): void
  toggleHidden(game: Game): void
  toggleFavorite(game: Game): void
  openFolder(game: Game): void
  remove(game: Game): void
  setEmulator(game: Game, routeIndex: number, emulatorId: string | null): Promise<void>
}

const emptyArt: GamePageArt = { hero: [], logo: [], custom: defaultGamePageCustom }

export function GamePage({ game, index, settings, running, libraryVersion, actions }: { game: Game; index: number; settings: Settings; running?: RunningGame; libraryVersion: number; actions: GamePageActions }) {
  const [art, setArt] = useState<GamePageArt>(emptyArt)
  const [heroIdx, setHeroIdx] = useState(0)
  const [logoIdx, setLogoIdx] = useState(0)
  const [sessions, setSessions] = useState<PlaySession[]>([])
  const [stats, setStats] = useState<PlayStats | null>(null)
  const [routeIdx, setRouteIdx] = useState(0)
  const [menuOpen, setMenuOpen] = useState(false)
  const [emulators, setEmulators] = useState<EmulatorProfile[]>([])
  const [catalog, setCatalog] = useState<EmulatorCatalog | null>(null)
  const [emuError, setEmuError] = useState('')
  const [customizing, setCustomizing] = useState(false)
  const [igdbPicking, setIgdbPicking] = useState(false)
  const { view: igdb, setView: setIgdb } = useIgdb(game.id)
  const { shots, setShots } = useScreenshots(game.id)
  const inputState = useGameInput(game.id)
  const { layout, change: changeLayout, flush: flushLayout } = useLogoLayout(game.id, art.custom)
  const now = useNow(!!running || sessions.some(session => !session.endedAt))

  const defaultRoute = useMemo(() => Math.max(0, game.routes.findIndex(route => route.installed)), [game.id]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { setRouteIdx(defaultRoute); setMenuOpen(false); setCustomizing(false); setIgdbPicking(false) }, [game.id, defaultRoute])

  useEffect(() => {
    let live = true
    setArt(emptyArt); setHeroIdx(0); setLogoIdx(0)
    const load = () => window.lobby.art.get(game.id).then(next => { if (live) setArt(next) }).catch(() => {})
    load()
    const off = window.lobby.art.onChanged(event => { if (event.gameId === game.id) { setHeroIdx(0); setLogoIdx(0); void load() } })
    return () => { live = false; off() }
  }, [game.id])
  const hasEmulatorRoute = game.routes.some(isEmulatorRoute)
  useEffect(() => {
    if (!hasEmulatorRoute) return
    let live = true
    window.lobby.emulators.list().then(next => { if (live) setEmulators(next) }).catch(() => {})
    if (!catalog) window.lobby.emulators.catalog().then(next => { if (live) setCatalog(next) }).catch(() => {})
    return () => { live = false }
  }, [game.id, hasEmulatorRoute, libraryVersion]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { setEmuError('') }, [game.id])
  const changeEmulator = (index: number, value: string) => {
    setEmuError('')
    actions.setEmulator(game, index, value === '' ? null : value).catch(error => setEmuError(error instanceof Error ? error.message : 'Could not change the emulator'))
  }
  const closeCustomize = useCallback(() => { flushLayout(); setCustomizing(false) }, [flushLayout])
  const closeIgdb = useCallback(() => setIgdbPicking(false), [])

  const runKey = running ? running.state + ':' + running.startedAt : 'idle'
  useEffect(() => {
    let live = true
    Promise.all([window.lobby.tracking.sessions(game.id, 50), window.lobby.tracking.stats(game.id)])
      .then(([nextSessions, nextStats]) => { if (live) { setSessions(nextSessions); setStats(nextStats) } })
      .catch(() => {})
    return () => { live = false }
  }, [game.id, libraryVersion, runKey])

  const heroSrc = art.hero[heroIdx] ? coverUrl(art.hero[heroIdx]) : undefined
  const logoSrc = art.logo[logoIdx] ? coverUrl(art.logo[logoIdx]) : undefined
  const cover = useCover(game)
  const route = game.routes[routeIdx]
  const systemLabel = (item: GameRoute) => item.system ? catalog?.systems.find(system => system.id === item.system)?.name ?? item.system : undefined
  const launching = running?.state === 'launching'
  const isRunning = running?.state === 'running'
  const totalSeconds = stats?.totalSeconds ?? game.playtimeSeconds ?? game.playtimeMinutes * 60
  const lastPlayed = stats?.lastPlayed ?? game.lastPlayed
  const installFolder = game.routes.find(item => item.installPath)?.installPath
  const sourceLabel = (source: string) => source === 'launch' ? 'Launched from Game Lobby' : 'Detected'
  const hasLiveRow = sessions.some(item => !item.endedAt)
  const menuItems: Array<[string, () => void]> = [
    ['Edit metadata', () => actions.edit(game)],
    ['Customize page', () => setCustomizing(true)],
    ['Set custom cover', () => actions.setCover(game)],
    ['Find cover on SteamGridDB', () => actions.findCover(game)],
    ['Find details on IGDB', () => setIgdbPicking(true)],
    ['Link another game', () => actions.link(game)],
    [game.hidden ? 'Unhide' : 'Hide', () => actions.toggleHidden(game)],
    ['Open install folder', () => actions.openFolder(game)],
    ['Remove from library', () => actions.remove(game)]
  ]

  return <div className="game-page">
    <div className="gp-hero">
      {heroSrc
        ? <img key={heroSrc} className="gp-hero-img" src={heroSrc} alt="" onError={() => setHeroIdx(value => value + 1)} />
        : <div className="gp-hero-fallback" style={cover.src && cover.status === 'loaded' ? coverLayerStyle(cover.src) : coverStyle(game, index)} />}
      <div className="gp-hero-fade" />
      <button className="gp-back" onClick={actions.back}><ArrowLeft size={15} /> Library</button>
      <button className="gp-customize" onClick={() => setCustomizing(true)} aria-haspopup="dialog" title="Customize hero, logo and layout"><Pencil size={13} /> Customize</button>
      <div className={'gp-logo-wrap pos-' + layout.position} style={{ '--logo-scale': layout.scale } as React.CSSProperties}>
        {logoSrc
          ? <img key={logoSrc} className="gp-logo" src={logoSrc} alt={game.title} onError={() => setLogoIdx(value => value + 1)} />
          : <h1 className="gp-title">{game.title}</h1>}
      </div>
    </div>

    {igdbPicking && <IgdbPicker game={game} view={igdb} onClose={closeIgdb} onChosen={next => { setIgdb(next); setIgdbPicking(false) }} />}
    {customizing && <CustomizePanel game={game} custom={art.custom} heroSrc={heroSrc} logoSrc={logoSrc} layout={layout} onLayout={changeLayout} onClose={closeCustomize} sgdbSettingsKey={JSON.stringify([settings.steamGridDbAnimated, settings.steamGridDbNsfw, settings.steamGridDbHumor])} />}

    <div className="gp-bar">
      {launching
        ? <button className="gp-play" disabled><Play size={18} fill="currentColor" /> Launching…</button>
        : isRunning
          ? <button className="gp-play running" disabled><span className="gp-live-dot" /> Running <span className="gp-clock">{formatClock(elapsedSeconds(running.startedAt, now))}</span></button>
          : <button className="gp-play" onClick={() => actions.play(game, routeIdx)}><Play size={18} fill="currentColor" /> Play</button>}
      {game.routes.length > 0 && <label className="gp-route">
        <span className="gp-route-label">Play from</span>
        <span className="gp-route-select">
          {route && <StoreIcon store={route.store} size={14} />}
          <select value={routeIdx} onChange={event => setRouteIdx(Number(event.target.value))} aria-label="Play from">
            {game.routes.map((item, i) => <option key={item.sourceId + '-' + i} value={i} title={routeTitle(item, game.routes, systemLabel(item))}>{routeLabel(item, game.routes)}{item.installed ? '' : ' (not installed)'}</option>)}
          </select>
          <ChevronDown size={13} />
        </span>
      </label>}
      <div className="gp-stats">
        <div className="gp-stat"><span>Last played</span><b>{formatDate(lastPlayed)}</b></div>
        <div className="gp-stat"><span>Play time</span><b>{formatTotal(totalSeconds)}</b></div>
        <div className="gp-stat"><span>Last two weeks</span><b>{formatTotal(stats?.lastTwoWeeksSeconds ?? 0)}</b></div>
      </div>
      <div className={'gp-track' + (!settings.tracking ? ' off' : running ? ' on' : '')}
        title={running ? (running.exe ? 'Tracking ' + running.exe + ' · ' : '') + (running.source === 'detected' ? 'detected running' : 'launched from Game Lobby') : undefined}>
        {settings.tracking && running && <span className="gp-live-dot" />}
        {!settings.tracking ? 'Tracking off' : running ? 'Tracking' : 'Not running'}
      </div>
      <ControllerBadge input={inputState.input} />
      <button className={'gp-icon' + (game.favorite ? ' active' : '')} aria-pressed={game.favorite} title={game.favorite ? 'Remove from favorites' : 'Add to favorites'} onClick={() => actions.toggleFavorite(game)}><Heart size={18} fill={game.favorite ? 'currentColor' : 'none'} /></button>
      <div className="gp-more">
        <button className="gp-icon" title="More actions" aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen(value => !value)}><MoreHorizontal size={18} /></button>
        {menuOpen && <>
          <div className="menu-dismiss gp-dismiss" onMouseDown={() => setMenuOpen(false)} />
          <div className="context-menu gp-menu" role="menu">
            {menuItems.map(([label, run]) => <button key={label} role="menuitem" className={label.startsWith('Remove') ? 'danger' : ''} onClick={() => { setMenuOpen(false); run() }}>{label}</button>)}
          </div>
        </>}
      </div>
    </div>

    <div className="gp-body">
      <section className="gp-activity">
        <h2>Activity</h2>
        {sessions.length === 0 && !isRunning
          ? <div className="gp-empty">No play sessions recorded yet. Sessions appear here once you play{settings.tracking ? '.' : ' (tracking is off in settings).'}</div>
          : <ul className="gp-sessions">
            {running && isRunning && !hasLiveRow && <li className="live">
              <div><b>{formatDate(running.startedAt)}</b><small>{formatTime(running.startedAt)}</small></div>
              <span className="gp-tag">{sourceLabel(running.source)}</span>
              <span className="gp-dur"><span className="gp-live-dot" /> {formatClock(elapsedSeconds(running.startedAt, now))}</span>
            </li>}
            {sessions.map(session => {
              const inProgress = !session.endedAt
              return <li key={session.id} className={inProgress ? 'live' : ''}>
                <div><b>{formatDate(session.startedAt)}</b><small>{formatTime(session.startedAt)}</small></div>
                <span className="gp-tag">{sourceLabel(session.source)}</span>
                <span className="gp-dur">{inProgress ? <><span className="gp-live-dot" /> {formatClock(elapsedSeconds(session.startedAt, now))}</> : formatSession(session.seconds)}</span>
              </li>
            })}
          </ul>}
        <div className="gp-summary">
          <div><span>Sessions</span><b>{stats?.sessionCount ?? 0}</b></div>
          <div><span>First played</span><b>{formatDate(stats?.firstPlayed)}</b></div>
        </div>
        <IgdbAbout view={igdb} title={game.title} onSearch={() => setIgdbPicking(true)} />
        <GameNotesSection gameId={game.id} shots={shots} />
      </section>

      <ImageWall game={game} igdb={igdb} shots={shots} onShots={setShots} />

      <aside className="gp-side">
        <div className={'gp-poster cover-' + cover.status} style={coverStyle(game, index)}><CoverLayer src={cover.src} status={cover.status} />{cover.status !== 'loaded' && <span>{game.title}</span>}</div>
        <div className="gp-side-block"><div className="detail-label">STORES</div>
          {game.routes.length === 0 && <div className="muted-text">No launch routes</div>}
          {game.routes.map((item, i) => <div className="gp-route-row" key={item.sourceId + '-' + i}>
            <span className="store-badge small"><StoreIcon store={item.store} /></span>
            <span className="route-name"><span className="route-label" title={routeTitle(item, game.routes, systemLabel(item))}>{routeLabel(item, game.routes)}</span><small>{item.installed ? 'Installed' : item.owned ? 'Owned · not installed' : 'Launch route'}</small></span>
          </div>)}
          {game.routes.map((item, i) => {
            if (!isEmulatorRoute(item)) return null
            const systemName = item.system ? catalog?.systems.find(system => system.id === item.system)?.name ?? item.system : 'Unknown system'
            const candidates = emulators.filter(profile => profile.enabled !== false && !!item.system && (profile.systems ?? []).includes(item.system))
            const assigned = item.emulatorId ? emulators.find(profile => profile.id === item.emulatorId) : undefined
            const options = assigned && !candidates.some(profile => profile.id === assigned.id) ? [...candidates, assigned] : candidates
            return <div className="gp-emulator" key={'emu-' + item.sourceId + '-' + i}>
              <label>
                <span className="detail-label">EMULATOR · {systemName.toUpperCase()}</span>
                <select aria-label={'Emulator for ' + systemName} value={assigned ? assigned.id : ''} onChange={event => changeEmulator(i, event.target.value)}>
                  <option value="">Default (automatic)</option>
                  {options.map(profile => <option key={profile.id} value={profile.id}>{profile.name}{profile.enabled === false ? ' (disabled)' : profile.available === false ? ' (not found)' : profile.needsConfiguration ? ' (needs setup)' : ''}</option>)}
                </select>
              </label>
              {!item.system && <div className="inline-warning">System unknown. Choose an emulator to play this ROM.</div>}
              {item.system && candidates.length === 0 && <div className="inline-warning">No emulator is configured for {systemName}. Add an emulator folder or profile in Settings, then pick the system there.</div>}
            </div>
          })}
          {emuError && <div className="form-error" role="alert">{emuError}</div>}
        </div>
        <GenreBlock game={game} view={igdb} />
        <YourScore gameId={game.id} />
        <IgdbRatings view={igdb} />
        <ControllerBlock gameId={game.id} state={inputState} />
        <div className="gp-side-block"><div className="detail-label">INSTALL FOLDER</div>
          <div className="muted-text gp-path">{installFolder || 'Unknown'}</div>
          <button className="secondary-button gp-folder" onClick={() => actions.openFolder(game)}><FolderOpen size={14} /> Open folder</button>
        </div>
        <div className="gp-side-block"><div className="detail-label">PLAYTIME</div>
          <div className="gp-totals"><div><span>Total</span><b>{formatTotal(totalSeconds)}</b></div><div><span>Last two weeks</span><b>{formatTotal(stats?.lastTwoWeeksSeconds ?? 0)}</b></div></div>
        </div>
      </aside>
    </div>
  </div>
}
