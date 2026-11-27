import { useEffect, useMemo, useRef, useState } from 'react'
import { ArrowDown, ArrowLeft, ArrowUp, Gamepad2 } from 'lucide-react'
import type { StatsSummary, StatsTopGame } from '../shared/types'
import { formatBytes } from '../shared/storage'
import { barWidth, defaultDirection, formatPercent, playtimeBreakdown, sortTop, storeHint, type SortDirection, type TopSortKey } from '../shared/stats'
import { coverSrc, cssUrl } from './cover'
import { formatTotal } from './format'

type Tab = 'Overview' | 'Completion' | 'Top playtime' | 'Library'
const tabs: Tab[] = ['Overview', 'Completion', 'Top playtime', 'Library']
const HIDDEN_KEY = 'gameLobby.statsIncludeHidden'

function storedHidden(): boolean {
  try { return window.localStorage.getItem(HIDDEN_KEY) === '1' } catch { return false }
}

const plural = (n: number, one: string, many = one + 's') => n.toLocaleString() + ' ' + (n === 1 ? one : many)

function Tile({ label, value, note, title, lead }: { label: string; value: string; note?: string; title?: string; lead?: boolean }) {
  return <div className={'stat-tile' + (lead ? ' lead' : '')} title={title}><div className="stat-label">{label}</div><div className="stat-value">{value}</div>{note && <div className="stat-note">{note}</div>}</div>
}

function Empty({ title, text }: { title: string; text: string }) {
  return <div className="stats-empty"><Gamepad2 size={28} /><h3>{title}</h3><p>{text}</p></div>
}

/** Row of a bar list: name, a plain CSS bar, a count and a share. */
function BarRow({ name, count, percent, width, tone }: { name: string; count: string; percent: number; width: number; tone?: 'muted' }) {
  return <div className="stat-bar-row" role="listitem">
    <span className="stat-bar-name" title={name}>{name}</span>
    <span className="stat-bar-track" aria-hidden><span className={'stat-bar-fill' + (tone ? ' ' + tone : '')} style={{ width: width + '%' }} /></span>
    <span className="stat-bar-count">{count}</span>
    <span className="stat-bar-percent">{formatPercent(percent)}</span>
  </div>
}

function TopTable({ data, onOpen }: { data: StatsSummary; onOpen: (gameId: string) => void }) {
  const [key, setKey] = useState<TopSortKey>('seconds')
  const [direction, setDirection] = useState<SortDirection>('desc')
  const rows = useMemo(() => sortTop(data.top, key, direction), [data.top, key, direction])
  const sortBy = (next: TopSortKey) => {
    if (next === key) setDirection(direction === 'asc' ? 'desc' : 'asc')
    else { setKey(next); setDirection(defaultDirection(next)) }
  }
  const header = (next: TopSortKey, label: string, className: string) => {
    const active = key === next
    return <div className={'top-head ' + className} role="columnheader" aria-sort={active ? (direction === 'asc' ? 'ascending' : 'descending') : 'none'}>
      <button onClick={() => sortBy(next)} title={'Sort by ' + label.toLowerCase()}>{label}{active && (direction === 'asc' ? <ArrowUp size={12} /> : <ArrowDown size={12} />)}</button>
    </div>
  }
  if (!data.top.length) return <Empty title="No play time yet" text="Games you play from GameLobbies, and Steam or GOG playtime once an account is set up under Settings, show up here." />
  const biggest = Math.max(...rows.map(row => row.percent), 0)
  return <div className="top-table" role="table" aria-label="Top play time">
    <div className="top-row top-header" role="row">
      {header('title', 'Game', 'top-game')}
      {header('seconds', 'Time played', 'top-time')}
      {header('seconds', '% of total', 'top-percent')}
    </div>
    {rows.map((row, index) => <TopRow key={row.gameId} row={row} biggest={biggest} lead={index === 0} onOpen={onOpen} />)}
    <p className="stats-foot">Top {data.top.length} of {plural(data.playtime.playedGames, 'game')} with play time. The percentage is the share of the total play time ({formatTotal(data.playtime.totalSeconds)}).</p>
  </div>
}

function TopRow({ row, biggest, lead, onOpen }: { row: StatsTopGame; biggest: number; lead: boolean; onOpen: (gameId: string) => void }) {
  const src = coverSrc(row.cover)
  const fromStore = row.storeSeconds !== undefined && row.storeSeconds > row.trackedSeconds
  return <div className="top-row" role="row">
    <div className="top-game" role="cell">
      <span className="top-cover" style={src ? { backgroundImage: cssUrl(src) } : undefined} aria-hidden />
      <button className="top-title" onClick={() => onOpen(row.gameId)} title={'Open ' + row.title}><strong>{row.title}</strong><small>{row.store}</small></button>
    </div>
    <div className="top-time" role="cell" title={playtimeBreakdown(row, formatTotal)}>
      {formatTotal(row.seconds)}{fromStore && <small>{row.storeSource ?? 'Store'}</small>}
    </div>
    <div className="top-percent" role="cell">
      <span className="stat-bar-track" aria-hidden><span className={'stat-bar-fill' + (lead ? '' : ' muted')} style={{ width: barWidth(row.percent, biggest) + '%' }} /></span>
      <span>{formatPercent(row.percent)}</span>
    </div>
  </div>
}

/**
 * Library statistics, shown in place of the library like Settings: a Back control and Escape
 * (handled by App) leave it. Everything is computed by the backend (`stats.summary`); this only
 * formats it.
 */
export function StatsView({ onClose, onOpenGame, say }: { onClose(): void; onOpenGame(gameId: string): void; say(message: string): void }) {
  const [tab, setTab] = useState<Tab>('Overview')
  const [includeHidden, setIncludeHidden] = useState(storedHidden)
  const [data, setData] = useState<StatsSummary | null>(null)
  const sayRef = useRef(say)
  sayRef.current = say

  useEffect(() => {
    let current = true
    const load = () => window.lobby.stats.summary(includeHidden)
      .then(next => { if (current) setData(next) })
      .catch(error => { if (current) sayRef.current(error instanceof Error ? error.message : 'Could not read statistics') })
    void load()
    const off = window.lobby.library.onChanged(() => void load())
    return () => { current = false; off() }
  }, [includeHidden])

  const toggleHidden = (value: boolean) => {
    setIncludeHidden(value)
    try { window.localStorage.setItem(HIDDEN_KEY, value ? '1' : '0') } catch { /* not persisted */ }
  }
  const moveTab = (event: React.KeyboardEvent, index: number) => {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0
    if (!step) return
    event.preventDefault()
    const next = tabs[(index + step + tabs.length) % tabs.length]
    setTab(next)
    document.getElementById('stats-tab-' + next)?.focus()
  }

  const hint = data ? storeHint(data.storeSources) : ''
  const empty = !!data && data.counts.all === 0
  const body = () => {
    if (!data) return <div className="stats-loading" role="status">Loading statistics…</div>
    if (empty) return <Empty title="No games to count" text={!includeHidden && data.counts.hidden ? 'Every game in your library is hidden. Turn on Include hidden games to count them.' : 'Scan your stores or add a game, then come back.'} />
    const { counts, playtime, installSize, completion, library } = data
    if (tab === 'Overview') return <>
      <div className="stat-tiles">
        <Tile label="All games" value={counts.all.toLocaleString()} />
        <Tile label="Installed" value={counts.installed.toLocaleString()} />
        <Tile label="Not installed" value={counts.notInstalled.toLocaleString()} />
        <Tile label="Hidden" value={counts.hidden.toLocaleString()} note={includeHidden ? 'counted in the figures' : 'not counted in the figures'} />
        <Tile label="Favorites" value={counts.favorite.toLocaleString()} />
        <Tile label="Total play time" value={formatTotal(playtime.totalSeconds)} note={hint || undefined} lead />
        <Tile label="Average play time" value={playtime.playedGames ? formatTotal(playtime.averageSeconds) : '–'} note={'per game with play time (' + playtime.playedGames.toLocaleString() + ')'} title="Total play time divided by the games you have played for at least a moment. Unplayed games are not averaged in." />
        <Tile label="Total install size" value={installSize.known ? formatBytes(installSize.bytes) : '–'} note={installSize.unknown ? plural(installSize.unknown, 'installed game') + ' with unknown size' + (installSize.known ? ', not counted' : '') : installSize.partial ? 'some sizes are lower bounds' : undefined} title="Sum of the known sizes of installed games. Rescan sizes in Storage to measure more." />
      </div>
      {hint && playtime.storeSeconds > 0 && <p className="stats-foot">Play time {hint}: {formatTotal(playtime.storeSeconds)} of the total is hours Steam or GOG report beyond what GameLobbies tracked. Per game, the larger of the two counts, so time is never counted twice.</p>}
    </>
    if (tab === 'Completion') {
      const total = completion.played + completion.notPlayed
      return <div className="stats-card">
        <div className="stat-stack" role="img" aria-label={`${completion.played} played, ${completion.notPlayed} not played`}>
          <span className="stat-bar-fill" style={{ width: barWidth(completion.played, total) + '%' }} />
        </div>
        <div className="stat-bars" role="list">
          <BarRow name="Played" count={plural(completion.played, 'game')} percent={total ? completion.played / total * 100 : 0} width={barWidth(completion.played, total)} />
          <BarRow name="Not played" count={plural(completion.notPlayed, 'game')} percent={total ? completion.notPlayed / total * 100 : 0} width={barWidth(completion.notPlayed, total)} tone="muted" />
        </div>
        <p className="stats-foot">Played: the game has play time (tracked, or reported by a store), a last-played date or a recorded session. Not played: everything else.</p>
      </div>
    }
    if (tab === 'Top playtime') return <TopTable data={data} onOpen={onOpenGame} />
    const most = Math.max(...library.map(entry => entry.count), 0)
    return <div className="stats-card">
      <div className="stat-bars" role="list">
        {library.map((entry, index) => <BarRow key={entry.name} name={entry.name} count={plural(entry.count, 'game')} percent={entry.percent} width={barWidth(entry.count, most)} tone={index === 0 ? undefined : 'muted'} />)}
      </div>
      <p className="stats-foot">Each game is counted once, under the store or launcher shown on its card, even when it has several ways to play. The percentage is the share of the {plural(counts.all, 'game')} counted.</p>
    </div>
  }

  return <div className="settings-view stats-view">
    <header className="settings-head">
      <button className="back-button" onClick={onClose} aria-label="Back to the library" title="Back (Esc)"><ArrowLeft size={16} /></button>
      <span className="meta-spacer" />
      <label className="stats-toggle" title="Hidden games are left out of every figure unless this is on"><input type="checkbox" checked={includeHidden} onChange={event => toggleHidden(event.target.checked)} />Include hidden games</label>
    </header>
    <div className="stats-body">
      <h1>Statistics</h1>
      <div className="stats-tabs" role="tablist" aria-label="Statistics sections">
        {tabs.map((name, index) => <button key={name} id={'stats-tab-' + name} role="tab" aria-selected={tab === name} aria-controls="stats-panel" tabIndex={tab === name ? 0 : -1} className={tab === name ? 'active' : ''} onClick={() => setTab(name)} onKeyDown={event => moveTab(event, index)}>{name}</button>)}
      </div>
      <div className="stats-panel" id="stats-panel" role="tabpanel" aria-labelledby={'stats-tab-' + tab} key={tab}>{body()}</div>
    </div>
  </div>
}
