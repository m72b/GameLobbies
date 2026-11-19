import { useCallback, useRef, useState } from 'react'
import { previewKind } from '../shared/preview'
import { Spinner } from './loading-image'
import type { SteamGridDbCover, SteamGridDbCoverPage } from '../shared/types'

export type SgdbPickerKind = 'grid' | 'hero' | 'logo'
export type SgdbPickerState = 'idle' | 'loading' | 'ready' | 'nokey' | 'error'

/** Highest page the backend accepts (0-based). */
const MAX_PAGE = 50
/** Pages already fetched this session, by kind, game, search settings and page number. */
const pageCache = new Map<string, SteamGridDbCoverPage>()

function fetchPage(kind: SgdbPickerKind, gameId: string, page: number) {
  const api = window.lobby.steamGridDb
  return kind === 'hero' ? api.searchHeroes(gameId, page) : kind === 'logo' ? api.searchLogos(gameId, page) : api.search(gameId, page)
}

/**
 * Paged SteamGridDB results for a picker: start() loads page 0, loadMore() appends the next page
 * (one request per call, none while one is running). Pages are cached for the session per
 * `settingsKey` (the search filters), so reopening a picker is instant.
 */
export function useSgdbPages(kind: SgdbPickerKind, gameId: string, settingsKey = '') {
  const [state, setState] = useState<SgdbPickerState>('idle')
  const [covers, setCovers] = useState<SteamGridDbCover[]>([])
  const [page, setPage] = useState(-1)
  const [hasMore, setHasMore] = useState(false)
  const [loadingMore, setLoadingMore] = useState(false)
  const [error, setError] = useState('')
  const run = useRef(0)
  const busy = useRef(false)

  const load = useCallback(async (next: number, id: number) => {
    const key = JSON.stringify([kind, gameId, settingsKey, next])
    const result = pageCache.get(key) ?? await fetchPage(kind, gameId, next)
    pageCache.set(key, result)
    if (id !== run.current) return
    setCovers(current => {
      const seen = new Set(next === 0 ? [] : current.map(cover => cover.id))
      const fresh = result.covers.filter(cover => !seen.has(cover.id))
      return next === 0 ? fresh : [...current, ...fresh]
    })
    setPage(next)
    setHasMore(result.hasMore && next < MAX_PAGE)
  }, [kind, gameId, settingsKey])

  const reset = useCallback(() => {
    run.current++
    busy.current = false
    setState('idle'); setCovers([]); setPage(-1); setHasMore(false); setLoadingMore(false); setError('')
  }, [])

  const start = useCallback(async () => {
    const id = ++run.current
    busy.current = true
    setState('loading'); setCovers([]); setPage(-1); setHasMore(false); setLoadingMore(false); setError('')
    try {
      const { keySet } = await window.lobby.steamGridDb.status()
      if (id !== run.current) return
      if (!keySet) { setState('nokey'); return }
      await load(0, id)
      if (id === run.current) setState('ready')
    } catch (e) {
      if (id !== run.current) return
      setError(e instanceof Error ? e.message : 'Could not search SteamGridDB'); setState('error')
    } finally { if (id === run.current) busy.current = false }
  }, [load])

  const loadMore = useCallback(async () => {
    if (busy.current || !hasMore) return
    const id = run.current
    busy.current = true
    setLoadingMore(true); setError('')
    try { await load(page + 1, id) }
    catch (e) { if (id === run.current) setError(e instanceof Error ? e.message : 'Could not load more from SteamGridDB') }
    finally { if (id === run.current) { busy.current = false; setLoadingMore(false) } }
  }, [hasMore, load, page])

  return { state, covers, hasMore, loadingMore, error, start, loadMore, reset }
}

/** "Showing N" with a Load more button, or a note that there is nothing more. */
export function LoadMore({ shown, hasMore, loading, onMore, noun }: { shown: number; hasMore: boolean; loading: boolean; onMore(): void; noun: string }) {
  return <div className="sgdb-more" role="status" aria-live="polite">
    <span>Showing {shown} {noun}{shown === 1 ? '' : 's'}</span>
    {hasMore
      ? <button type="button" className="secondary-button" onClick={onMore} disabled={loading}>{loading ? 'Loading…' : 'Load more'}</button>
      : <span className="sgdb-more-end">No more results on SteamGridDB</span>}
  </div>
}

/** Candidates the picker can show with the user's animated setting (previews it can't play are left out). */
export function pickerCovers(covers: SteamGridDbCover[], animated: boolean) {
  return covers.filter(cover => previewKind(cover.thumb, animated) !== null)
}

function CoverTile({ cover, kind, disabled, onApply }: { cover: SteamGridDbCover; kind: 'image' | 'video'; disabled: boolean; onApply(cover: SteamGridDbCover): void }) {
  const [state, setState] = useState<'loading' | 'ready' | 'failed'>('loading')
  const label = (kind === 'video' ? 'Animated cover option' : 'Cover option') + (cover.author ? ' by ' + cover.author : ' ' + cover.id)
  const ready = () => setState('ready')
  const failed = () => setState('failed')
  return <button type="button" className={'sgdb-tile ' + state} disabled={disabled} onClick={() => onApply(cover)} aria-label={label} title={cover.author ? 'By ' + cover.author : undefined}>
    {state === 'loading' && <span className="sgdb-skeleton" aria-hidden="true"><Spinner /></span>}
    {state === 'failed' && <span className="sgdb-tile-note" aria-hidden="true">Preview unavailable</span>}
    {kind === 'video'
      ? <video
          src={cover.thumb}
          ref={video => { if (video) video.muted = true }}
          muted
          loop
          autoPlay
          playsInline
          preload="auto"
          disablePictureInPicture
          aria-hidden="true"
          onLoadedData={event => { ready(); void event.currentTarget.play().catch(() => undefined) }}
          onError={failed}
        />
      : <img src={cover.thumb} alt="" decoding="async" draggable={false} onLoad={ready} onError={failed} />}
    {kind === 'video' && <span className="sgdb-badge">Animated</span>}
  </button>
}

/** The SteamGridDB cover grid: fixed 2:3 tiles with a skeleton until each preview has loaded. */
export function CoverTiles({ covers, animated, applying, onApply }: { covers: SteamGridDbCover[]; animated: boolean; applying: boolean; onApply(cover: SteamGridDbCover): void }) {
  return <div className={'sgdb-grid' + (applying ? ' busy' : '')} aria-busy={applying}>
    {covers.map(cover => {
      const kind = previewKind(cover.thumb, animated)
      return kind && <CoverTile key={cover.id} cover={cover} kind={kind} disabled={applying} onApply={onApply} />
    })}
  </div>
}
