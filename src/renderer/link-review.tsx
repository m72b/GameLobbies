import { useEffect, useState } from 'react'
import { X } from 'lucide-react'
import { coverUrl } from './lobby-api'
import { formatTotal } from './format'
import { withRouteName } from '../shared/routes'
import { FadeImg } from './loading-image'
import type { Game, LinkChoices, LinkPreview, LinkPreviewGame } from '../shared/types'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

type ChoiceKey = 'coverFrom' | 'heroFrom' | 'notesFrom' | 'igdbFrom'

function ChoiceGroup({ legend, games, value, onChange, describe, name }: { legend: string; games: LinkPreviewGame[]; value: string; onChange(id: string): void; describe(game: LinkPreviewGame): string; name: string }) {
  return <fieldset className="review-choices"><legend>{legend}</legend>
    {games.map(game => <label key={game.id}><input type="radio" name={name} checked={value === game.id} onChange={() => onChange(game.id)} /> {describe(game)}</label>)}
  </fieldset>
}

/**
 * The review step before games are linked: what will be combined, how conflicting details are
 * resolved (with choices), and the route names "Play from" will show. Linking itself happens in
 * Rust (`library.link`) with game ids and these choices only.
 */
export function LinkReview({ gameIds, reviewId, onClose, onLinked }: { gameIds: string[]; reviewId?: string; onClose(): void; onLinked(games: Game[], keptId: string): void }) {
  const [preview, setPreview] = useState<LinkPreview | null>(null)
  const [choices, setChoices] = useState<LinkChoices | null>(null)
  const [custom, setCustom] = useState('')
  const [useCustom, setUseCustom] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const idsKey = gameIds.join('\n')

  // The suggested route names depend on the shared title: ask again when it changes.
  const sharedTitle = (next: LinkChoices | null, p: LinkPreview | null) => useCustom ? custom.trim() : p?.games.find(game => game.id === next?.titleFrom)?.title
  const load = (title?: string) => window.lobby.library.linkPreview(gameIds, title || undefined)
  useEffect(() => {
    let live = true
    setPreview(null); setChoices(null); setError('')
    load().then(next => { if (live) { setPreview(next); setChoices(next.defaults) } }).catch(caught => { if (live) setError(errorText(caught, 'Could not prepare the link')) })
    return () => { live = false }
  }, [idsKey]) // eslint-disable-line react-hooks/exhaustive-deps
  const refresh = (next: LinkChoices, title?: string) => {
    load(title ?? sharedTitle(next, preview)).then(p => setPreview(current => current ? { ...current, routes: p.routes } : p)).catch(() => {})
  }
  const set = (patch: Partial<LinkChoices>) => setChoices(current => current ? { ...current, ...patch } : current)

  const submit = async () => {
    if (!choices) return
    setBusy(true); setError('')
    try {
      const games = await window.lobby.library.link(gameIds, { ...choices, customTitle: useCustom && custom.trim() ? custom.trim() : undefined }, reviewId)
      onLinked(games, choices.keep)
    } catch (caught) { setError(errorText(caught, 'Could not link these games')) }
    finally { setBusy(false) }
  }

  const games = preview?.games ?? []
  const byId = (id: string) => games.find(game => game.id === id)
  const withHero = games.filter(game => game.hasHero)
  const withNotes = games.filter(game => game.hasNotes)
  const withIgdb = games.filter(game => game.igdbName)
  const titles = games.filter((game, index) => games.findIndex(other => other.title === game.title) === index)
  const pick = (key: ChoiceKey) => (id: string) => set({ [key]: id } as Partial<LinkChoices>)

  return <div className="modal-scrim"><div className="modal link-modal link-review" role="dialog" aria-label="Review link">
    <div className="modal-head"><h2>Link {gameIds.length} games</h2><button type="button" aria-label="Close" onClick={onClose}><X size={17} /></button></div>
    {!preview && !error && <div className="empty-inline">Preparing…</div>}
    {error && <div className="form-error" role="alert">{error}</div>}
    {preview && choices && <div className="link-review-body">
      <p>They become one game with every launch route. Each route keeps its own store, target, install state and playtime. Nothing is deleted: details you don’t pick are kept, and come back if you unlink an entry later.</p>
      <ul className="review-games">{games.map(game => <li key={game.id}><b>{game.title}</b> · {game.store} · {formatTotal(game.playtimeSeconds)} · {game.sessions} session{game.sessions === 1 ? '' : 's'}{game.linked > 0 ? ` · ${game.linked} linked already` : ''}{game.favorite ? ' · ★' : ''}</li>)}</ul>

      <ChoiceGroup legend="Keep which entry? The others are linked into it." name="link-keep" games={games} value={choices.keep} onChange={id => set({ keep: id })} describe={game => game.title + ' · ' + game.store} />

      <fieldset className="review-choices"><legend>Title</legend>
        {titles.map(game => <label key={game.id}><input type="radio" name="link-title" checked={!useCustom && choices.titleFrom === game.id} onChange={() => { setUseCustom(false); const next = { ...choices, titleFrom: game.id }; setChoices(next); refresh(next, game.title) }} /> {game.title}</label>)}
        <label><input type="radio" name="link-title" checked={useCustom} onChange={() => setUseCustom(true)} /> <input className="link-custom-title" value={custom} maxLength={300} placeholder="Another title" onFocus={() => setUseCustom(true)} onChange={event => setCustom(event.target.value)} onBlur={() => { if (useCustom && custom.trim()) refresh(choices, custom.trim()) }} /></label>
      </fieldset>

      {preview.conflicts.cover
        ? <fieldset className="review-choices"><legend>Cover</legend>
          <div className="link-covers">{games.map(game => <label key={game.id} className={'link-cover' + (choices.coverFrom === game.id ? ' chosen' : '')}><input type="radio" name="link-cover" checked={choices.coverFrom === game.id} onChange={() => set({ coverFrom: game.id })} />{game.cover ? <FadeImg src={coverUrl(game.cover)} alt="" /> : <span className="link-cover-none">No cover</span>}<small>{game.title}{game.customCover ? ' · yours' : ''}</small></label>)}</div>
        </fieldset>
        : <p className="muted-text">Cover: {byId(choices.coverFrom)?.title ?? 'the kept entry'}’s.</p>}
      {withHero.length > 1 && <ChoiceGroup legend="Game page art (hero and logo)" name="link-hero" games={withHero} value={choices.heroFrom} onChange={pick('heroFrom')} describe={game => game.title} />}
      {withNotes.length > 1 && <ChoiceGroup legend="Your score and notes" name="link-notes" games={withNotes} value={choices.notesFrom} onChange={pick('notesFrom')} describe={game => game.title} />}
      {withIgdb.length > 1 && preview.conflicts.igdb && <ChoiceGroup legend="IGDB details" name="link-igdb" games={withIgdb} value={choices.igdbFrom} onChange={pick('igdbFrom')} describe={game => (game.igdbName ?? '') + ' (for ' + game.title + ')' + (game.igdbChosen ? ' · chosen by you' : '')} />}
      <label className="toggle-row"><span>Favorite{preview.conflicts.favorite ? ' (one of them is)' : ''}</span><input type="checkbox" checked={choices.favorite} onChange={event => set({ favorite: event.target.checked })} /></label>

      <div className="detail-label">Play from after linking</div>
      <ul className="review-games link-routes">{preview.routes.map((route, index) => <li key={route.sourceId + index}>{withRouteName(route.store, route)}{route.suggested ? <span className="status-pill">suggested name</span> : null}<small>{route.installed ? ' · installed' : route.owned ? ' · owned' : ''} · from {byId(route.gameId)?.title ?? 'another entry'}</small></li>)}</ul>
      <p className="muted-text">Rename routes (for example “Java Edition”) on the game page after linking. In total: {formatTotal(preview.playtimeSeconds)} played, {preview.sessions} session{preview.sessions === 1 ? '' : 's'}; screenshots, memories and sessions all move to the kept entry.</p>
    </div>}
    <div className="modal-actions"><button type="button" className="secondary-button" onClick={onClose}>Cancel</button><button type="button" className="primary-button" disabled={!choices || busy || (useCustom && !custom.trim())} onClick={() => void submit()}>{busy ? 'Linking…' : 'Link games'}</button></div>
  </div></div>
}
