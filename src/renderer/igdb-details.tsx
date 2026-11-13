// IGDB details on the game page: description, companies, release date, ratings, genres and
// the "Wrong game? Search IGDB" picker. Everything IGDB sends is rendered as plain React text.
import { useCallback, useEffect, useState } from 'react'
import { X } from 'lucide-react'
import type { Game, IgdbMetadataView, IgdbRating, IgdbSearchResult } from '../shared/types'

const emptyView: IgdbMetadataView = { configured: false, pending: false, checked: false }

/** IGDB release dates are UTC midnight; formatting in UTC keeps the right day. */
const formatRelease = (ms?: number) => ms === undefined ? '' : new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' })
const releaseYear = (ms?: number) => ms === undefined ? '' : String(new Date(ms).getUTCFullYear())

/** Cached metadata for a game, updated live when a background fetch stores new data. */
export function useIgdb(gameId: string) {
  const [view, setView] = useState<IgdbMetadataView>(emptyView)
  useEffect(() => {
    let live = true
    setView(emptyView)
    window.lobby.igdb.get(gameId).then(next => { if (live) setView(next) }).catch(() => {})
    const off = window.lobby.igdb.onMetadata(event => {
      if (!live || event.gameId !== gameId) return
      if (event.changed) window.lobby.igdb.get(gameId).then(next => { if (live) setView(next) }).catch(() => {})
      else setView(current => ({ ...current, pending: false }))
    })
    return () => { live = false; off() }
  }, [gameId])
  return { view, setView }
}

function names(list: string[]) {
  return list.length ? list.join(', ') : undefined
}

/** Left column, under Activity: description and facts. Renders nothing without data, except a quiet hint. */
export function IgdbAbout({ view, onSearch }: { view: IgdbMetadataView; onSearch(): void }) {
  const [more, setMore] = useState(false)
  const metadata = view.metadata
  useEffect(() => { setMore(false) }, [metadata?.igdbId])
  if (!metadata) {
    if (view.configured && view.checked && !view.pending) {
      return <div className="gp-igdb-hint muted-text">No IGDB match for this game.<button className="link-button" onClick={onSearch}>Search IGDB</button></div>
    }
    return null
  }
  const facts: Array<[string, string | undefined]> = [
    ['Developer', names(metadata.developers)],
    ['Publisher', names(metadata.publishers)],
    ['Released', formatRelease(metadata.releaseDate) || undefined]
  ]
  return <section className="gp-about">
    <h2>About</h2>
    {metadata.summary
      ? <p className="gp-about-text">{metadata.summary}</p>
      : <div className="muted-text">IGDB has no description for this game.</div>}
    {metadata.storyline && (more
      ? <><div className="detail-label gp-about-label">STORYLINE</div><p className="gp-about-text">{metadata.storyline}</p><button className="link-button gp-about-more" onClick={() => setMore(false)}>Less</button></>
      : <button className="link-button gp-about-more" onClick={() => setMore(true)}>More: storyline</button>)}
    {facts.some(([, value]) => value) && <div className="gp-summary gp-facts">
      {facts.map(([label, value]) => value ? <div key={label}><span>{label}</span><b>{value}</b></div> : null)}
    </div>}
    <div className="gp-igdb-links">
      {metadata.url && <a className="external-link" href={metadata.url}>View on IGDB</a>}
      <button className="link-button" onClick={onSearch} title={view.source === 'user' ? 'You chose this IGDB entry' : 'Matched automatically'}>Wrong game? Search IGDB</button>
    </div>
  </section>
}

function ratingText(rating: IgdbRating, noun: string) {
  return rating.count ? rating.count.toLocaleString() + ' ' + noun + (rating.count === 1 ? '' : 's') : undefined
}

/** Side column block: critic and user ratings (each omitted when IGDB has none). */
export function IgdbRatings({ view }: { view: IgdbMetadataView }) {
  const critic = view.metadata?.criticRating
  const user = view.metadata?.userRating
  if (!critic && !user) return null
  return <div className="gp-side-block"><div className="detail-label">RATING · IGDB</div>
    <div className="gp-totals">
      {critic && <div title="IGDB aggregated critic rating"><span>Critics</span><b>{critic.score}<small className="gp-rating-max">/100</small></b>{ratingText(critic, 'review') && <small className="gp-rating-count">{ratingText(critic, 'review')}</small>}</div>}
      {user && <div title="IGDB user rating"><span>Users</span><b>{user.score}<small className="gp-rating-max">/100</small></b>{ratingText(user, 'rating') && <small className="gp-rating-count">{ratingText(user, 'rating')}</small>}</div>}
    </div>
  </div>
}

/** The GENRE block: the user's genre stays first; IGDB's genres are shown alongside (never saved). */
export function GenreBlock({ game, view }: { game: Game; view: IgdbMetadataView }) {
  const userGenre = game.genre?.trim()
  const igdbGenres = (view.metadata?.genres ?? []).filter(genre => genre.toLocaleLowerCase() !== userGenre?.toLocaleLowerCase())
  return <div className="gp-side-block"><div className="detail-label">GENRE</div>
    {userGenre ? <div className="muted-text">{userGenre}</div> : igdbGenres.length === 0 && <div className="muted-text">Not set</div>}
    {igdbGenres.length > 0 && <div className="chip-group compact" title="Genres from IGDB">{igdbGenres.map(genre => <span key={genre} className="chip static">{genre}</span>)}</div>}
  </div>
}

/** "Wrong game? Search IGDB": search by title or free text and pick the right entry. */
export function IgdbPicker({ game, view, onClose, onChosen }: { game: Game; view: IgdbMetadataView; onClose(): void; onChosen(view: IgdbMetadataView): void }) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<IgdbSearchResult[] | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const run = useCallback(async (text?: string) => {
    setBusy(true); setError('')
    try { setResults(await window.lobby.igdb.search(game.id, text?.trim() || undefined)) }
    catch (reason) { setResults([]); setError(reason instanceof Error ? reason.message : 'IGDB search failed') }
    finally { setBusy(false) }
  }, [game.id])
  useEffect(() => { if (view.configured) void run() }, [run, view.configured])
  useEffect(() => {
    // Capture phase so the page-level Escape (back to library) does not also fire.
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.stopImmediatePropagation(); event.preventDefault()
      onClose()
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [onClose])
  const choose = async (result: IgdbSearchResult) => {
    setBusy(true); setError('')
    try { onChosen(await window.lobby.igdb.choose(game.id, result.igdbId)) }
    catch (reason) { setError(reason instanceof Error ? reason.message : 'Could not use that IGDB entry') }
    finally { setBusy(false) }
  }
  const reset = async () => {
    setBusy(true); setError('')
    try { onChosen(await window.lobby.igdb.resetMatch(game.id)) }
    catch (reason) { setError(reason instanceof Error ? reason.message : 'Could not reset the match') }
    finally { setBusy(false) }
  }
  const current = view.metadata?.igdbId
  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget) onClose() }}>
    <div className="modal link-modal" role="dialog" aria-label={'Find ' + game.title + ' on IGDB'}>
      <div className="modal-head"><h2>Find on IGDB</h2><button type="button" aria-label="Close" onClick={onClose}><X size={17} /></button></div>
      {!view.configured
        ? <div className="empty-inline">Add your IGDB (Twitch) Client ID and Client Secret in settings to search IGDB.<br /><button className="secondary-button" onClick={() => void window.lobby.settings.open()}>Open settings</button></div>
        : <>
          <p>Choose the IGDB entry for <b>{game.title}</b>. Your choice is kept for this game.</p>
          <form onSubmit={event => { event.preventDefault(); void run(query) }}>
            <label>Search IGDB<input value={query} onChange={event => setQuery(event.target.value)} placeholder={game.title} maxLength={200} autoFocus spellCheck={false} /></label>
          </form>
          {error && <div className="form-error" role="alert">{error}</div>}
          <div className="link-game-list">
            {busy && results === null && <div className="empty-inline">Searching IGDB…</div>}
            {results?.map(result => <button className="link-game-row" key={result.igdbId} disabled={busy} onClick={() => void choose(result)}>
              <span><b>{result.name}</b><small>{[releaseYear(result.releaseDate), result.platforms.slice(0, 6).join(', '), result.mainGame ? '' : 'DLC / edition'].filter(Boolean).join(' · ') || 'IGDB #' + result.igdbId}</small></span>
              <span>{result.igdbId === current ? 'Current' : 'Use this'}</span>
            </button>)}
            {results !== null && results.length === 0 && !error && <div className="empty-inline">No IGDB results. Try a different search.</div>}
          </div>
        </>}
      <div className="modal-actions">
        {view.source === 'user' && <button className="secondary-button" disabled={busy} onClick={() => void reset()}>Use automatic match</button>}
        <button className="secondary-button" onClick={onClose}>Cancel</button>
      </div>
    </div>
  </div>
}
