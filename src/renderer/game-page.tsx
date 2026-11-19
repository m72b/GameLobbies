import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { ArrowLeft, ChevronDown, FolderOpen, Heart, MoreHorizontal, Pencil, Play, SlidersHorizontal, Unlink } from 'lucide-react'
import { LauncherIcon, RouteIcon } from './launcher-icon'
import { CustomizePanel, useLogoLayout } from './customize-page'
import { coverUrl } from './lobby-api'
import { defaultGamePageCustom } from '../shared/defaults'
import { coverLayerStyle, coverStyle } from './cover'
import { CoverLayer, useCover } from './poster'
import { FadeImg } from './loading-image'
import { zoomIn, type ZoomOrigin } from './zoom'
import { GenreBlock, IgdbAbout, IgdbPicker, IgdbRatings, useIgdb } from './igdb-details'
import { argsSupport, argsText, isEmulatorRoute, routeLabel, routeTitle, withRouteName } from '../shared/routes'
import { launcherOfRoute, playLauncher } from '../shared/launchers'
import { LauncherBlock } from './launcher-assign'
import { ImageWall, useScreenshots } from './image-wall'
import { GameNotesSection, YourScore } from './game-notes'
import { ControllerBadge, ControllerBlock, useGameInput } from './controller-info'
import { SessionHistory } from './session-history'
import { formatRange } from '../shared/sessions'
import { addedTooltip, additionOf, mergeActivity } from '../shared/store-time'
import { SteamAddedText, SteamTag } from './steam-added'
import { elapsedSeconds, formatClock, formatDate, formatSession, formatTime, formatTotal, useNow } from './format'
import { LaunchOptionsDialog, RouteToolPanel, type RouteTool } from './route-tools'
import type { ArgsPreview, CustomLauncher, EmulatorCatalog, EmulatorProfile, Game, GamePageArt, GameRoute, PlaySession, PlayStats, RunningGame, Settings, UnlinkPreview } from '../shared/types'

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
  /** Opens "Link to launcher" for the game. */
  linkLauncher(game: Game): void
  /** Removes only what linking the game to that launcher added. */
  unassignLauncher(game: Game, launcherId: string): Promise<void>
  /** Names a route (display only); empty clears it. */
  renameRoute(game: Game, routeIndex: number, name: string): Promise<void>
  /** Launch options for a route (Rust checks them and confirms natively). */
  setRouteArgs(game: Game, routeIndex: number, args: string): Promise<void>
  /** The route's stored launch options need confirming before they run. */
  routeArgsPending(game: Game, routeIndex: number): Promise<boolean>
  /** Checks launch options text for a route without saving (what would run, or the reason it is refused). */
  routeArgsCheck(game: Game, routeIndex: number, args: string): Promise<ArgsPreview>
  unlinkPreview(game: Game, routeIndex: number): Promise<UnlinkPreview>
  /** Splits a route (and the routes that came in with it) off into its own game. */
  unlink(game: Game, routeIndex: number): Promise<void>
}

const emptyArt: GamePageArt = { hero: [], logo: [], custom: defaultGamePageCustom }

export function GamePage({ game, index, settings, launchers, running, libraryVersion, actions, origin }: { origin?: ZoomOrigin | null; game: Game; index: number; settings: Settings; launchers: CustomLauncher[]; running?: RunningGame; libraryVersion: number; actions: GamePageActions }) {
  const pageRef = useRef<HTMLDivElement>(null)
  // Opening animation (zoom from the clicked poster); runs once, when the page first appears.
  const openedFrom = useRef(origin ?? null)
  useLayoutEffect(() => (pageRef.current ? zoomIn(pageRef.current, openedFrom.current) : undefined), [])
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
  const [historyOpen, setHistoryOpen] = useState(false)
  const [tool, setTool] = useState<{ index: number; tool: RouteTool } | null>(null)
  const [optionsOpen, setOptionsOpen] = useState(false)
  const [argsPending, setArgsPending] = useState(false)
  const [argsError, setArgsError] = useState('')
  const [argsBusy, setArgsBusy] = useState(false)
  const toggleTool = (index: number, next: RouteTool) => setTool(current => current && current.index === index && current.tool === next ? null : { index, tool: next })
  const { view: igdb, setView: setIgdb } = useIgdb(game.id)
  const { shots, setShots } = useScreenshots(game.id)
  const inputState = useGameInput(game.id)
  const { layout, change: changeLayout, flush: flushLayout } = useLogoLayout(game.id, art.custom)
  const now = useNow(!!running || sessions.some(session => !session.endedAt))

  // Routes added or removed (a launcher assigned on this page goes first) pick the default again,
  // so the selection never points at a route that moved.
  const routesKey = game.routes.map(route => route.sourceId + '|' + route.launchTarget).join('\n')
  const defaultRoute = useMemo(() => Math.max(0, game.routes.findIndex(route => route.installed)), [game.id, routesKey]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { setMenuOpen(false); setCustomizing(false); setIgdbPicking(false) }, [game.id])
  useEffect(() => { setRouteIdx(defaultRoute); setTool(null) }, [game.id, routesKey, defaultRoute])
  // Whether the chosen route's stored launch options need confirming before Play uses them (Rust
  // decides: they came from a backup or an older version, or the program or command line changed).
  const chosen = game.routes[routeIdx]
  useEffect(() => {
    setArgsError('')
    if (!chosen?.launchArgs) { setArgsPending(false); return }
    let live = true
    actions.routeArgsPending(game, routeIdx).then(next => { if (live) setArgsPending(next) }).catch(() => { if (live) setArgsPending(false) })
    return () => { live = false }
  }, [game.id, routeIdx, chosen?.sourceId, chosen?.launchTarget, chosen?.launchArgs, launchers, libraryVersion]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { setOptionsOpen(false) }, [game.id, routeIdx])
  const confirmArgs = async () => {
    if (!chosen) return
    setArgsBusy(true); setArgsError('')
    try { await actions.setRouteArgs(game, routeIdx, argsText(chosen.launchArgs)); setArgsPending(false) }
    catch (caught) { setArgsError(caught instanceof Error ? caught.message : 'Could not confirm the launch options') }
    finally { setArgsBusy(false) }
  }

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
  const playVia = playLauncher(route, launchers)
  // A route run by or labelled with a launcher reads as that launcher ("Play from: My Arcade";
  // the store once the launcher is deleted), with the file when the launcher has several routes here.
  // A route's own name (e.g. "Java Edition") follows the store or launcher, so two routes of one
  // store read apart: "Xbox · Java Edition", "Xbox · Bedrock Edition".
  const label = (item: GameRoute) => {
    const launcher = launcherOfRoute(item, launchers)
    if (!launcher) return routeLabel(item, game.routes)
    if (item.name) return withRouteName(launcher.name, item)
    const several = game.routes.filter(other => launcherOfRoute(other, launchers)?.id === launcher.id).length > 1
    const file = item.launchTarget.split(/[\\/]/).filter(Boolean).pop()
    return several && file ? launcher.name + ' · ' + file : launcher.name
  }
  const labelTitle = (item: GameRoute) => { const launcher = launcherOfRoute(item, launchers); return launcher ? withRouteName(launcher.name, item) + ' · ' + item.launchTarget : routeTitle(item, game.routes, systemLabel(item)) }
  const systemLabel = (item: GameRoute) => item.system ? catalog?.systems.find(system => system.id === item.system)?.name ?? item.system : undefined
  const launching = running?.state === 'launching'
  const isRunning = running?.state === 'running'
  const totalSeconds = stats?.totalSeconds ?? game.playtimeSeconds ?? game.playtimeMinutes * 60
  const lastPlayed = stats?.lastPlayed ?? game.lastPlayed
  const installFolder = game.routes.find(item => item.installPath)?.installPath
  const sourceLabel = (source: string) => source === 'launch' ? 'Launched from Game Lobby' : 'Detected'
  const hasLiveRow = sessions.some(item => !item.endedAt)
  // Steam's hours that Game Lobby did not track: one read-only entry, never a session.
  const steamEntry = additionOf(game, stats?.store)
  const activityRows = mergeActivity(sessions, steamEntry ? [steamEntry] : [])
  const menuItems: Array<[string, () => void]> = [
    ['Edit metadata', () => actions.edit(game)],
    ['Customize page', () => setCustomizing(true)],
    ['Set custom cover', () => actions.setCover(game)],
    ['Find cover on SteamGridDB', () => actions.findCover(game)],
    ['Find details on IGDB', () => setIgdbPicking(true)],
    ['Link other games…', () => actions.link(game)],
    ...(launchers.length > 0 ? [['Link to launcher…', () => actions.linkLauncher(game)] as [string, () => void]] : []),
    [game.hidden ? 'Unhide' : 'Hide', () => actions.toggleHidden(game)],
    ['Open install folder', () => actions.openFolder(game)],
    ['Remove from library', () => actions.remove(game)]
  ]

  return <div className="game-page" ref={pageRef}>
    <div className="gp-hero">
      {heroSrc
        ? <FadeImg key={heroSrc} className="gp-hero-img" src={heroSrc} alt="" onError={() => setHeroIdx(value => value + 1)} />
        : <div className="gp-hero-fallback" style={cover.src && cover.status === 'loaded' ? coverLayerStyle(cover.src) : coverStyle(game, index)} />}
      <div className="gp-hero-fade" />
      <button className="gp-back" onClick={actions.back}><ArrowLeft size={15} /> Library</button>
      <button className="gp-customize" onClick={() => setCustomizing(true)} aria-haspopup="dialog" title="Customize hero, logo and layout"><Pencil size={13} /> Customize</button>
      <div className={'gp-logo-wrap pos-' + layout.position} style={{ '--logo-scale': layout.scale } as React.CSSProperties}>
        {logoSrc
          ? <FadeImg key={logoSrc} className="gp-logo" src={logoSrc} alt={game.title} onError={() => setLogoIdx(value => value + 1)} />
          : <h1 className="gp-title">{game.title}</h1>}
      </div>
    </div>

    {igdbPicking && <IgdbPicker game={game} view={igdb} onClose={closeIgdb} onChosen={next => { setIgdb(next); setIgdbPicking(false) }} />}
    {customizing && <CustomizePanel game={game} custom={art.custom} heroSrc={heroSrc} logoSrc={logoSrc} layout={layout} onLayout={changeLayout} onClose={closeCustomize} sgdbSettingsKey={JSON.stringify([settings.steamGridDbAnimated, settings.steamGridDbNsfw, settings.steamGridDbHumor])} animated={settings.steamGridDbAnimated} />}

    <div className="gp-bar">
      {launching
        ? <button className="gp-play" disabled><Play size={18} fill="currentColor" /> Launching…</button>
        : isRunning
          ? <button className="gp-play running" disabled><span className="gp-live-dot" /> Running <span className="gp-clock">{formatClock(elapsedSeconds(running.startedAt, now))}</span></button>
          : <button className="gp-play" onClick={() => actions.play(game, routeIdx)} title={route ? 'Play from ' + labelTitle(route) : undefined}><Play size={18} fill="currentColor" /> Play{playVia
            ? <span className="gp-play-via"><LauncherIcon launcher={playVia} size={12} /> {route ? label(route) : playVia.name}</span>
            : route && (game.routes.length > 1 || route.name) && <span className="gp-play-via">{label(route)}</span>}</button>}
      {game.routes.length > 0 && <div className="gp-route">
        <label className="gp-route-pick">
          <span className="gp-route-label">Play from</span>
          <span className="gp-route-select">
            {route && <RouteIcon route={route} launchers={launchers} size={14} />}
            <select value={routeIdx} onChange={event => setRouteIdx(Number(event.target.value))} aria-label="Play from">
              {game.routes.map((item, i) => <option key={item.sourceId + '-' + i} value={i} title={labelTitle(item)}>{label(item)}{item.installed ? '' : ' (not installed)'}</option>)}
            </select>
            <ChevronDown size={13} />
          </span>
        </label>
        {route && argsSupport(route, launchers).ok && <div className="gp-route-options">
          {route.launchArgs
            ? <span className="gp-options-text" title={argsText(route.launchArgs)}>Options: <code>{argsText(route.launchArgs)}</code></span>
            : null}
          {route.launchArgs && argsPending && <><span className="gp-options-pending">Needs confirmation</span><button type="button" className="gp-options-button strong" disabled={argsBusy} onClick={() => void confirmArgs()}>Confirm…</button></>}
          <button type="button" className="gp-options-button" onClick={() => setOptionsOpen(true)}>{route.launchArgs ? 'Edit' : <><SlidersHorizontal size={11} /> Launch options</>}</button>
        </div>}
        {argsError && <div className="gp-options-error" role="alert">{argsError}</div>}
      </div>}
      {optionsOpen && route && (() => {
        const support = argsSupport(route, launchers)
        return support.ok && <LaunchOptionsDialog key={routeIdx + '|' + route.launchTarget} title={game.title} routeLabel={label(route)} route={route} support={support} pending={argsPending}
          onSave={args => actions.setRouteArgs(game, routeIdx, args)} onCheck={args => actions.routeArgsCheck(game, routeIdx, args)} onClose={() => setOptionsOpen(false)} />
      })()}
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
        <div className="gp-activity-head"><h2>Activity</h2><button type="button" className="secondary-button" onClick={() => setHistoryOpen(true)}>All sessions</button></div>
        {historyOpen && <SessionHistory gameId={game.id} title={game.title} running={running ? [running] : []} onClose={() => setHistoryOpen(false)} />}
        {sessions.length === 0 && !isRunning && !steamEntry
          ? <div className="gp-empty">No play sessions recorded yet. Sessions appear here once you play{settings.tracking ? '.' : ' (tracking is off in settings).'}</div>
          : <ul className="gp-sessions">
            {running && isRunning && !hasLiveRow && <li className="live">
              <div><b>{formatDate(running.startedAt)}</b><small>{formatTime(running.startedAt)}</small></div>
              <span className="gp-tag">{sourceLabel(running.source)}</span>
              <span className="gp-dur"><span className="gp-live-dot" /> {formatClock(elapsedSeconds(running.startedAt, now))}</span>
            </li>}
            {activityRows.map(row => {
              if (row.kind === 'steam') return <li key="steam-added" className="gp-steam-row" title={addedTooltip(row.entry)}>
                <SteamAddedText entry={row.entry} />
                <SteamTag />
                <span className="gp-dur" />
              </li>
              const session = row.session
              const inProgress = !session.endedAt
              // Only a session the tracker is running now counts up; any other keeps its saved length.
              const live = inProgress && isRunning
              return <li key={session.id} className={inProgress ? 'live' : ''}>
                <div><b>{formatDate(session.startedAt)}</b><small>{formatRange(session.startedAt, session.endedAt, formatTime)}{session.interrupted ? ' · Interrupted' : ''}{session.edited ? ' · Edited' : ''}</small></div>
                <span className="gp-tag">{sourceLabel(session.source)}</span>
                <span className="gp-dur">{live ? <><span className="gp-live-dot" /> {formatClock(elapsedSeconds(session.startedAt, now))}</> : formatSession(session.seconds)}</span>
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
          {game.routes.map((item, i) => <div key={item.sourceId + '-' + i}>
            <div className="gp-route-row">
              <span className="store-badge small"><RouteIcon route={item} launchers={launchers} /></span>
              <span className="route-name"><span className="route-label" title={labelTitle(item)}>{label(item)}</span><small>{item.installed ? 'Installed' : item.owned ? 'Owned · not installed' : 'Launch route'}{item.launchArgs ? ' · launch options' : ''}</small></span>
              <span className="gp-route-actions">
                <button type="button" title="Name this route" aria-label={'Rename ' + label(item)} onClick={() => toggleTool(i, 'name')}><Pencil size={12} /></button>
                <button type="button" title="Launch options" aria-label={'Launch options for ' + label(item)} onClick={() => toggleTool(i, 'args')}><SlidersHorizontal size={12} /></button>
                {game.routes.length > 1 && <button type="button" title="Unlink into its own game" aria-label={'Unlink ' + label(item)} onClick={() => toggleTool(i, 'unlink')}><Unlink size={12} /></button>}
              </span>
            </div>
            {tool && tool.index === i && <RouteToolPanel key={tool.tool + i} tool={tool.tool} route={item} launchers={launchers} canUnlink={game.routes.length > 1}
              onRename={name => actions.renameRoute(game, i, name)} onArgs={args => actions.setRouteArgs(game, i, args)} onArgsPending={() => actions.routeArgsPending(game, i)} onArgsCheck={args => actions.routeArgsCheck(game, i, args)}
              onUnlinkPreview={() => actions.unlinkPreview(game, i)} onUnlink={() => actions.unlink(game, i)} onClose={() => setTool(null)} />}
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
        <LauncherBlock game={game} launchers={launchers} libraryVersion={libraryVersion} onOpenLink={() => actions.linkLauncher(game)} onUnassign={launcherId => actions.unassignLauncher(game, launcherId)} />
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
