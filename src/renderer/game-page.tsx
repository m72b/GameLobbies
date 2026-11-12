import { useEffect, useMemo, useState } from 'react'
import { ArrowLeft, ChevronDown, FolderOpen, Heart, MoreHorizontal, Play } from 'lucide-react'
import { StoreIcon } from './store-icons'
import { coverImage, coverStyle } from './cover'
import { elapsedSeconds, formatClock, formatDate, formatSession, formatTime, formatTotal, useNow } from './format'
import type { Game, GameArt, PlaySession, PlayStats, RunningGame, Settings } from '../shared/types'

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
}

const emptyArt: GameArt = { hero: [], logo: [] }

export function GamePage({ game, index, settings, running, libraryVersion, actions }: { game: Game; index: number; settings: Settings; running?: RunningGame; libraryVersion: number; actions: GamePageActions }) {
  const [art, setArt] = useState<GameArt>(emptyArt)
  const [heroIdx, setHeroIdx] = useState(0)
  const [logoIdx, setLogoIdx] = useState(0)
  const [sessions, setSessions] = useState<PlaySession[]>([])
  const [stats, setStats] = useState<PlayStats | null>(null)
  const [routeIdx, setRouteIdx] = useState(0)
  const [menuOpen, setMenuOpen] = useState(false)
  const now = useNow(!!running || sessions.some(session => !session.endedAt))

  const defaultRoute = useMemo(() => Math.max(0, game.routes.findIndex(route => route.installed)), [game.id]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => { setRouteIdx(defaultRoute); setMenuOpen(false) }, [game.id, defaultRoute])

  useEffect(() => {
    let live = true
    setArt(emptyArt); setHeroIdx(0); setLogoIdx(0)
    window.lobby.art.get(game.id).then(next => { if (live) setArt(next) }).catch(() => {})
    return () => { live = false }
  }, [game.id])

  const runKey = running ? running.state + ':' + running.startedAt : 'idle'
  useEffect(() => {
    let live = true
    Promise.all([window.lobby.tracking.sessions(game.id, 50), window.lobby.tracking.stats(game.id)])
      .then(([nextSessions, nextStats]) => { if (live) { setSessions(nextSessions); setStats(nextStats) } })
      .catch(() => {})
    return () => { live = false }
  }, [game.id, libraryVersion, runKey])

  const heroSrc = art.hero[heroIdx]
  const logoSrc = art.logo[logoIdx]
  const coverBg = coverImage(game.cover)
  const route = game.routes[routeIdx]
  const launching = running?.state === 'launching'
  const isRunning = running?.state === 'running'
  const totalSeconds = stats?.totalSeconds ?? game.playtimeSeconds ?? game.playtimeMinutes * 60
  const lastPlayed = stats?.lastPlayed ?? game.lastPlayed
  const installFolder = game.routes.find(item => item.installPath)?.installPath
  const sourceLabel = (source: string) => source === 'launch' ? 'Launched from Game Lobby' : 'Detected'
  const hasLiveRow = sessions.some(item => !item.endedAt)
  const menuItems: Array<[string, () => void]> = [
    ['Edit metadata', () => actions.edit(game)],
    ['Set custom cover', () => actions.setCover(game)],
    ['Find cover on SteamGridDB', () => actions.findCover(game)],
    ['Link another game', () => actions.link(game)],
    [game.hidden ? 'Unhide' : 'Hide', () => actions.toggleHidden(game)],
    ['Open install folder', () => actions.openFolder(game)],
    ['Remove from library', () => actions.remove(game)]
  ]

  return <div className="game-page">
    <div className="gp-hero">
      {heroSrc
        ? <img key={heroSrc} className="gp-hero-img" src={heroSrc} alt="" onError={() => setHeroIdx(value => value + 1)} />
        : <div className="gp-hero-fallback" style={coverBg ? { backgroundImage: coverBg } : coverStyle(game, index)} />}
      <div className="gp-hero-fade" />
      <button className="gp-back" onClick={actions.back}><ArrowLeft size={15} /> Library</button>
      <div className="gp-logo-wrap">
        {logoSrc
          ? <img key={logoSrc} className="gp-logo" src={logoSrc} alt={game.title} onError={() => setLogoIdx(value => value + 1)} />
          : <h1 className="gp-title">{game.title}</h1>}
      </div>
    </div>

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
            {game.routes.map((item, i) => <option key={item.sourceId + '-' + i} value={i}>{item.store}{item.installed ? '' : ' (not installed)'}</option>)}
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
      </section>

      <aside className="gp-side">
        <div className="gp-poster" style={coverStyle(game, index)}>{!game.cover && <span>{game.title}</span>}</div>
        <div className="gp-side-block"><div className="detail-label">STORES</div>
          {game.routes.length === 0 && <div className="muted-text">No launch routes</div>}
          {game.routes.map((item, i) => <div className="gp-route-row" key={item.sourceId + '-' + i}>
            <span className="store-badge small"><StoreIcon store={item.store} /></span>
            <span className="route-name">{item.store}<small>{item.installed ? 'Installed' : item.owned ? 'Owned · not installed' : 'Launch route'}</small></span>
          </div>)}
        </div>
        <div className="gp-side-block"><div className="detail-label">GENRE</div><div className="muted-text">{game.genre || 'Not set'}</div></div>
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
