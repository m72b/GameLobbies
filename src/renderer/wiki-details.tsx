// The built-in Wikidata / Wikipedia / Wikimedia Commons provider on the game page: the cached
// view (updated live), image credits, and "Find on Wikidata" for choosing the right item.
// Everything Wikimedia sends is rendered as plain React text; links open in the browser through
// the app's external-link allow list (Wikipedia, Wikidata, Commons, Creative Commons only).
import { useCallback, useEffect, useState } from 'react'
import { X } from 'lucide-react'
import type { Game, WikiCandidate, WikiImage, WikiView } from '../shared/types'
import { creditText } from '../shared/about'

export const emptyWikiView: WikiView = { enabled: false, pending: false, checked: false, candidates: [], logos: [], covers: [] }

/** Cached Wikimedia data for a game, updated when a background lookup stores new data. */
export function useWikimedia(gameId: string) {
  const [view, setView] = useState<WikiView>(emptyWikiView)
  useEffect(() => {
    let live = true
    setView(emptyWikiView)
    const load = () => window.lobby.wikimedia.get(gameId).then(next => { if (live) setView(next) }).catch(() => {})
    void load()
    const off = window.lobby.wikimedia.onMetadata(event => {
      if (!live || event.gameId !== gameId) return
      if (event.changed) void load()
      else setView(current => ({ ...current, pending: false }))
    })
    return () => { live = false; off() }
  }, [gameId])
  return { view, setView }
}

/** "Logo: Valve, Public domain via Wikimedia Commons", linking to the file page and the licence. */
export function ImageCredit({ image, className }: { image: WikiImage; className?: string }) {
  return <span className={'wiki-credit' + (className ? ' ' + className : '')} title={image.restrictions ? 'Restrictions: ' + image.restrictions : undefined}>
    <a className="external-link" href={image.sourcePage}>{creditText(image)}</a>
    {image.licenceUrl && <> · <a className="external-link" href={image.licenceUrl}>Licence</a></>}
  </span>
}

/** "From Wikipedia · <article> · CC BY-SA 4.0", shown under a Wikipedia description. */
export function WikipediaCredit({ url, title, licence }: { url?: string; title?: string; licence?: { name: string; url: string } }) {
  return <span className="wiki-credit">From Wikipedia{url && <> · <a className="external-link" href={url}>{title ? '“' + title + '”' : 'Article'}</a></>}{licence && <> · <a className="external-link" href={licence.url}>{licence.name}</a></>}</span>
}

const candidateLine = (c: WikiCandidate) => [c.description, c.year ? String(c.year) : ''].filter(Boolean).join(' · ') || c.qid

/** "Find on Wikidata": the ambiguous candidates first, then a search; the user's pick is kept. */
export function WikiPicker({ game, view, onClose, onChosen }: { game: Game; view: WikiView; onClose(): void; onChosen(view: WikiView): void }) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<WikiCandidate[] | null>(view.status === 'ambiguous' && view.candidates.length ? view.candidates : null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const run = useCallback(async (text?: string) => {
    setBusy(true); setError('')
    try { setResults(await window.lobby.wikimedia.search(game.id, text?.trim() || undefined)) }
    catch (reason) { setResults([]); setError(reason instanceof Error ? reason.message : typeof reason === 'string' ? reason : 'Wikidata search failed') }
    finally { setBusy(false) }
  }, [game.id])
  useEffect(() => { if (view.enabled && results === null) void run() }, [run, view.enabled]) // eslint-disable-line react-hooks/exhaustive-deps
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
  const act = async (job: () => Promise<WikiView>, fallback: string) => {
    setBusy(true); setError('')
    try { onChosen(await job()) }
    catch (reason) { setError(reason instanceof Error ? reason.message : typeof reason === 'string' ? reason : fallback) }
    finally { setBusy(false) }
  }
  const current = view.metadata?.qid
  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget) onClose() }}>
    <div className="modal link-modal" role="dialog" aria-label={'Find ' + game.title + ' on Wikidata'}>
      <div className="modal-head"><h2>Find on Wikidata</h2><button type="button" aria-label="Close" onClick={onClose}><X size={17} /></button></div>
      {!view.enabled
        ? <div className="empty-inline">Wikipedia and Wikidata are turned off.<br /><button className="secondary-button" onClick={() => void window.lobby.settings.open('metadata')}>Open Settings</button></div>
        : <>
          <p>Choose the Wikidata item for <b>{game.title}</b>. Your choice is kept for this game. Facts from Wikidata (CC0).</p>
          {view.status === 'ambiguous' && <p className="muted-text">Several items match this title, so none was picked automatically.</p>}
          <form onSubmit={event => { event.preventDefault(); void run(query) }}>
            <label>Search Wikidata<input value={query} onChange={event => setQuery(event.target.value)} placeholder={game.title} maxLength={200} autoFocus spellCheck={false} /></label>
          </form>
          {error && <div className="form-error" role="alert">{error}</div>}
          <div className="link-game-list">
            {busy && results === null && <div className="empty-inline">Searching Wikidata…</div>}
            {results?.map(result => <button className="link-game-row" key={result.qid} disabled={busy} onClick={() => void act(() => window.lobby.wikimedia.choose(game.id, result.qid), 'Could not use that Wikidata item')}>
              <span><b>{result.label}</b><small>{candidateLine(result)}</small></span>
              <span>{result.qid === current ? 'Current' : 'Use this'}</span>
            </button>)}
            {results !== null && results.length === 0 && !error && <div className="empty-inline">No video games found on Wikidata. Try a different search.</div>}
          </div>
        </>}
      <div className="modal-actions">
        {view.source === 'user' && <button className="secondary-button" disabled={busy} onClick={() => void act(() => window.lobby.wikimedia.resetMatch(game.id), 'Could not reset the match')}>Use automatic match</button>}
        <button className="secondary-button" onClick={onClose}>Cancel</button>
      </div>
    </div>
  </div>
}
