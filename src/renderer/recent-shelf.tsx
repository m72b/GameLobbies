import { useCallback, useEffect, useMemo, useState } from 'react'
import { X } from 'lucide-react'
import type { Game, RecentGame } from '../shared/types'
import { coverStyle } from './cover'
import { CoverLayer, useCover } from './poster'

function ShelfCard({ game, index, onOpen, onDismiss }: { game: Game; index: number; onOpen: () => void; onDismiss: () => void }) {
  const { src, status } = useCover(game)
  return <article className="recent-card" onClick={onOpen}>
    <div className={'poster cover-' + status} style={{ ...coverStyle(game, index), aspectRatio: '2 / 3' }}>
      <CoverLayer src={src} status={status} />
      <div className="poster-vignette" />
      <div className="poster-title-mark">{status !== 'loaded' && game.title.toUpperCase()}</div>
      <button className="recent-dismiss" aria-label={'Dismiss ' + game.title} title="Seen it" onClick={event => { event.stopPropagation(); onDismiss() }}><X size={13} /></button>
    </div>
    <div className="card-title">{game.title}</div>
  </article>
}

/** Games first discovered in the last two weeks, newest first, each dismissible. Renders nothing when the shelf is empty. */
export function RecentShelf({ games, libraryVersion, onOpen }: { games: Game[]; libraryVersion: number; onOpen: (game: Game) => void }) {
  const [recent, setRecent] = useState<RecentGame[]>([])
  useEffect(() => {
    let alive = true
    window.lobby.recent.list().then(list => { if (alive) setRecent(list) }).catch(() => { /* the shelf is optional */ })
    return () => { alive = false }
  }, [libraryVersion])
  const byId = useMemo(() => new Map(games.map(game => [game.id, game])), [games])
  const shown = recent.map(item => byId.get(item.gameId)).filter((game): game is Game => !!game && !game.hidden)
  const dismiss = useCallback((gameId?: string) => {
    window.lobby.recent.dismiss(gameId).then(setRecent).catch(() => { /* keep the shelf as it was */ })
  }, [])
  if (!shown.length) return null
  return <section className="recent-shelf" aria-label="Recently added">
    <div className="recent-head"><h2>Recently added</h2><span className="recent-count">{shown.length}</span><span className="meta-spacer" /><button className="recent-clear" onClick={() => dismiss()}>Dismiss all</button></div>
    <div className="recent-row">{shown.map((game, index) => <ShelfCard key={game.id} game={game} index={index} onOpen={() => onOpen(game)} onDismiss={() => dismiss(game.id)} />)}</div>
  </section>
}
