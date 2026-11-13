// IGDB details on the game page: description, companies, release date, time to beat, pictures,
// ratings, genres and the "Wrong game? Search IGDB" picker. Everything IGDB sends is rendered as
// plain React text; pictures come only from URLs the backend built from validated image ids.
import { useCallback, useEffect, useState } from 'react'
import { ChevronLeft, ChevronRight, X } from 'lucide-react'
// Bundled (SIL Open Font License), never loaded from the network: regular and bold only.
import '@fontsource/quantico/400.css'
import '@fontsource/quantico/700.css'
import type { Game, IgdbImage, IgdbMetadataView, IgdbRating, IgdbSearchResult, IgdbTimeToBeat } from '../shared/types'
import { coverSrc, useCoverStatus } from './cover'
import { CoverLayer } from './poster'

const emptyView: IgdbMetadataView = { configured: false, pending: false, checked: false }

/** IGDB release dates are UTC midnight; formatting in UTC keeps the right day. */
const formatRelease = (ms?: number) => ms === undefined ? '' : new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' })
const releaseYear = (ms?: number) => ms === undefined ? '' : String(new Date(ms).getUTCFullYear())

/** IGDB's required attribution, shown wherever IGDB data appears. */
export function IgdbAttribution({ className, images }: { className?: string; images?: boolean }) {
  return <span className={'igdb-attribution' + (className ? ' ' + className : '')}>Game data from <a href="https://www.igdb.com/">IGDB.com</a>{images && <> · Images from <a href="https://www.igdb.com/">IGDB.com</a></>}</span>
}

/** HowLongToBeat has no API: only a search link (https, opened in the browser). */
const hltbSearch = (title: string) => 'https://howlongtobeat.com/?q=' + encodeURIComponent(title.slice(0, 200))

function TimeToBeat({ time, title }: { time: IgdbTimeToBeat; title: string }) {
  const tiles: Array<[string, string | undefined]> = [
    ['Main story', time.mainStory],
    ['Main + extras', time.mainExtras],
    ['Completionist', time.completionist]
  ]
  return <div className="gp-ttb">
    <div className="gp-ttb-head"><h3>Time to beat</h3>{time.count ? <small>{time.count.toLocaleString()} submission{time.count === 1 ? '' : 's'} on IGDB</small> : null}</div>
    <div className="gp-ttb-tiles">
      {tiles.map(([label, value]) => <div key={label} className={'gp-ttb-tile' + (value ? '' : ' empty')}><span>{label}</span><b>{value ?? '–'}</b></div>)}
    </div>
    <a className="external-link gp-ttb-link" href={hltbSearch(title)}>Search on HowLongToBeat</a>
  </div>
}

/** One thumbnail: a shimmer while loading, hidden when it fails. */
function Thumb({ image, index, onOpen }: { image: IgdbImage; index: number; onOpen(index: number): void }) {
  const src = coverSrc(image.thumb)
  const status = useCoverStatus(src)
  if (!src || status === 'error') return null
  return <button type="button" className="gp-shot" onClick={() => onOpen(index)} aria-label={'Open ' + image.kind + ' ' + (index + 1)}>
    <CoverLayer src={src} status={status} />
  </button>
}

/** Full-size picture with Esc to close and the arrow keys to move. */
function Lightbox({ images, index, onIndex, onClose }: { images: IgdbImage[]; index: number; onIndex(index: number): void; onClose(): void }) {
  const image = images[index]
  const src = coverSrc(image?.full)
  const status = useCoverStatus(src)
  const step = useCallback((delta: number) => onIndex((index + delta + images.length) % images.length), [index, images.length, onIndex])
  useEffect(() => {
    // Capture phase so the page-level Escape (back to library) does not also fire.
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
      else if (event.key === 'ArrowRight') step(1)
      else if (event.key === 'ArrowLeft') step(-1)
      else return
      event.stopImmediatePropagation(); event.preventDefault()
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [onClose, step])
  if (!image) return null
  return <div className="modal-scrim gp-lightbox" role="dialog" aria-label="Picture viewer" onMouseDown={event => { if (event.target === event.currentTarget) onClose() }}>
    <div className="gp-lightbox-frame">
      {status === 'loading' && <div className="cover-shimmer" aria-hidden="true" />}
      {src && <img key={src} src={src} alt={image.kind === 'artwork' ? 'Artwork' : 'Screenshot'} className={status === 'loaded' ? 'loaded' : ''} />}
      {status === 'error' && <div className="gp-lightbox-error">This picture could not be loaded.</div>}
    </div>
    {images.length > 1 && <>
      <button type="button" className="gp-lightbox-nav prev" aria-label="Previous picture" onClick={() => step(-1)}><ChevronLeft size={22} /></button>
      <button type="button" className="gp-lightbox-nav next" aria-label="Next picture" onClick={() => step(1)}><ChevronRight size={22} /></button>
    </>}
    <button type="button" className="gp-lightbox-close" aria-label="Close" onClick={onClose}><X size={18} /></button>
    <div className="gp-lightbox-meta">{index + 1} / {images.length} · <IgdbAttribution images /></div>
  </div>
}

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
export function IgdbAbout({ view, title, onSearch }: { view: IgdbMetadataView; title: string; onSearch(): void }) {
  const [more, setMore] = useState(false)
  const [open, setOpen] = useState<number | null>(null)
  const metadata = view.metadata
  const images = view.images ?? []
  useEffect(() => { setMore(false); setOpen(null) }, [metadata?.igdbId])
  const closeLightbox = useCallback(() => setOpen(null), [])
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
    {facts.some(([, value]) => value) && <div className="gp-facts">
      {facts.map(([label, value]) => value ? <div key={label}><span>{label}</span><b>{value}</b></div> : null)}
    </div>}
    {view.timeToBeat && <TimeToBeat time={view.timeToBeat} title={metadata.name || title} />}
    {images.length > 0 && <div className="gp-shots" role="list" aria-label="Screenshots and artwork">
      {images.map((image, index) => <Thumb key={image.thumb} image={image} index={index} onOpen={setOpen} />)}
    </div>}
    <div className="gp-igdb-links">
      {metadata.url && <a className="external-link" href={metadata.url}>View on IGDB</a>}
      <button className="link-button" onClick={onSearch} title={view.source === 'user' ? 'You chose this IGDB entry' : 'Matched automatically'}>Wrong game? Search IGDB</button>
      <IgdbAttribution images={images.length > 0} />
    </div>
    {open !== null && images.length > 0 && <Lightbox images={images} index={Math.min(open, images.length - 1)} onIndex={setOpen} onClose={closeLightbox} />}
  </section>
}

function ratingText(rating: IgdbRating, noun: string) {
  return rating.count ? rating.count.toLocaleString() + ' ' + noun + (rating.count === 1 ? '' : 's') : undefined
}

/** Score band for the colour cue: 75 and up, 50 to 74, below 50. */
const band = (score: number) => score >= 75 ? 'high' : score >= 50 ? 'mid' : 'low'

function ScoreTile({ label, title, rating, noun }: { label: string; title: string; rating: IgdbRating; noun: string }) {
  const count = ratingText(rating, noun)
  return <div className={'gp-score band-' + band(rating.score)} title={title}>
    <span>{label}</span>
    <b>{rating.score}<small>/100</small></b>
    {count && <small className="gp-score-count">{count}</small>}
  </div>
}

/** Side column block: critic and user ratings as score tiles (each omitted when IGDB has none). */
export function IgdbRatings({ view }: { view: IgdbMetadataView }) {
  const critic = view.metadata?.criticRating
  const user = view.metadata?.userRating
  if (!critic && !user) return null
  return <div className="gp-side-block"><div className="detail-label">RATING</div>
    <div className="gp-scores">
      {critic && <ScoreTile label="Critics" title="IGDB aggregated critic rating" rating={critic} noun="review" />}
      {user && <ScoreTile label="Users" title="IGDB user rating" rating={user} noun="rating" />}
    </div>
    <IgdbAttribution className="gp-side-attribution" />
  </div>
}

/** The GENRE block: the user's genre stays first; IGDB's genres are shown alongside (never saved). */
export function GenreBlock({ game, view }: { game: Game; view: IgdbMetadataView }) {
  const userGenre = game.genre?.trim()
  const igdbGenres = (view.metadata?.genres ?? []).filter(genre => genre.toLocaleLowerCase() !== userGenre?.toLocaleLowerCase())
  // The ratings block below carries the attribution when it is shown.
  const hasRatings = !!(view.metadata?.criticRating || view.metadata?.userRating)
  return <div className="gp-side-block"><div className="detail-label">GENRE</div>
    {userGenre ? <div className="muted-text">{userGenre}</div> : igdbGenres.length === 0 && <div className="muted-text">Not set</div>}
    {igdbGenres.length > 0 && <div className="chip-group compact" title="Genres from IGDB">{igdbGenres.map(genre => <span key={genre} className="chip static">{genre}</span>)}</div>}
    {igdbGenres.length > 0 && !hasRatings && <IgdbAttribution className="gp-side-attribution" />}
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
        ? <div className="empty-inline">Add your IGDB (Twitch) Client ID and Client Secret in settings to search IGDB.<br /><button className="secondary-button" onClick={() => void window.lobby.settings.open('metadata')}>Open settings</button></div>
        : <>
          <p>Choose the IGDB entry for <b>{game.title}</b>. Your choice is kept for this game. <IgdbAttribution /></p>
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
