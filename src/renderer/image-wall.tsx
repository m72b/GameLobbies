// The game page's picture wall: the user's screenshots first, then IGDB's screenshots and
// artworks, as a mosaic with one large feature tile. Clicking opens the shared lightbox.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ImagePlus, MoreHorizontal, X } from 'lucide-react'
import type { Game, IgdbMetadataView, Screenshot } from '../shared/types'
import { isSteamGame } from '../shared/routes'
import { layoutWall, visualOrder, wallColumns, type Placement } from '../shared/wall'
import { coverSrc, useCoverStatus } from './cover'
import { Spinner } from './loading-image'
import { IgdbAttribution, Lightbox, type LightboxImage } from './igdb-details'

/** Tiles shown before "Show all". */
const FIRST_TILES = 12

interface WallImage { key: string; thumb: string; full: string; igdb: boolean; aspect?: number; shot?: Screenshot; alt: string }
type Filter = 'all' | 'yours' | 'igdb'

export function useScreenshots(gameId: string) {
  const [shots, setShots] = useState<Screenshot[]>([])
  useEffect(() => {
    let live = true
    setShots([])
    const load = () => window.lobby.screenshots.list(gameId).then(next => { if (live) setShots(next) }).catch(() => {})
    void load()
    const off = window.lobby.screenshots.onChanged(event => { if (event.gameId === gameId) void load() })
    return () => { live = false; off() }
  }, [gameId])
  return { shots, setShots }
}

function Tile({ image, place, onOpen, onAspect, onFailed, onMenu }: { image: WallImage; place: Placement; onOpen(): void; onAspect(key: string, aspect: number): void; onFailed(key: string): void; onMenu?(x: number, y: number): void }) {
  const src = coverSrc(image.thumb)
  const status = useCoverStatus(src)
  const failed = !src || status === 'error'
  // A picture that can't be shown is taken out of the layout (the wall re-packs without it).
  useEffect(() => { if (failed) onFailed(image.key) }, [failed, image.key, onFailed])
  if (failed) return null
  // Placed explicitly (computed like `grid-auto-flow: dense`), so the order is known.
  const style = { gridColumn: place.col + ' / span ' + place.cols, gridRow: place.row + ' / span ' + place.rows }
  return <div className="wall-tile" style={style} onContextMenu={onMenu ? event => { event.preventDefault(); onMenu(event.clientX, event.clientY) } : undefined}>
    <button type="button" className="wall-open" onClick={onOpen} aria-label={'Open ' + image.alt}>
      {status !== 'loaded' && <div className="cover-shimmer" aria-hidden="true"><Spinner /></div>}
      <img src={src} alt="" draggable={false} className={status === 'loaded' ? 'loaded' : ''}
        onLoad={event => { const img = event.currentTarget; if (img.naturalWidth && img.naturalHeight) onAspect(image.key, img.naturalWidth / img.naturalHeight) }} />
    </button>
    {onMenu && <button type="button" className="wall-more" aria-label="Screenshot actions" aria-haspopup="menu" onClick={event => { const box = event.currentTarget.getBoundingClientRect(); onMenu(box.right, box.bottom) }}><MoreHorizontal size={16} /></button>}
  </div>
}

export function ImageWall({ game, igdb, shots, onShots }: { game: Game; igdb: IgdbMetadataView; shots: Screenshot[]; onShots(shots: Screenshot[]): void }) {
  const [note, setNote] = useState('')
  useEffect(() => {
    if (!note) return
    const timer = window.setTimeout(() => setNote(''), 5000)
    return () => window.clearTimeout(timer)
  }, [note])
  const say = setNote
  const [filter, setFilter] = useState<Filter>('all')
  const [showAll, setShowAll] = useState(false)
  const [open, setOpen] = useState<number | null>(null)
  const [aspects, setAspects] = useState<Record<string, number>>({})
  const [width, setWidth] = useState(0)
  const [busy, setBusy] = useState(false)
  const [menu, setMenu] = useState<{ x: number; y: number; shot: Screenshot } | null>(null)
  const [confirm, setConfirm] = useState<Screenshot | null>(null)
  const [grid, setGrid] = useState<HTMLDivElement | null>(null)
  const [failed, setFailed] = useState<ReadonlySet<string>>(new Set())
  const pendingFailures = useRef(new Set<string>())
  const failureTimer = useRef<number | undefined>(undefined)
  useEffect(() => { setFilter('all'); setShowAll(false); setOpen(null); setMenu(null); setConfirm(null); setNote('') }, [game.id])
  // New pictures (or restored files): give everything another chance.
  useEffect(() => { pendingFailures.current.clear(); setFailed(new Set()) }, [game.id, shots, igdb.images])
  useEffect(() => () => window.clearTimeout(failureTimer.current), [])
  /** Failures are collected and applied together, so the wall re-packs once per batch. */
  const onFailed = useCallback((key: string) => {
    pendingFailures.current.add(key)
    window.clearTimeout(failureTimer.current)
    failureTimer.current = window.setTimeout(() => {
      const batch = [...pendingFailures.current]
      pendingFailures.current.clear()
      setFailed(current => batch.every(item => current.has(item)) ? current : new Set([...current, ...batch]))
    }, 200)
  }, [])

  useEffect(() => {
    if (!grid) return
    const observer = new ResizeObserver(() => setWidth(grid.clientWidth))
    observer.observe(grid)
    setWidth(grid.clientWidth)
    return () => observer.disconnect()
  }, [grid])
  useEffect(() => {
    if (!menu && !confirm) return
    // Capture phase so the page-level Escape (back to library) does not also fire.
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.stopImmediatePropagation(); event.preventDefault()
      setMenu(null); setConfirm(null)
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  }, [menu, confirm])

  const yours: WallImage[] = useMemo(() => shots.filter(shot => !shot.missing).map(shot => ({
    key: 'shot:' + shot.id, thumb: shot.src, full: shot.src, igdb: false, shot,
    aspect: shot.width && shot.height ? shot.width / shot.height : undefined,
    alt: shot.source === 'steam' ? 'Steam screenshot' : 'Your screenshot'
  })), [shots])
  const fromIgdb: WallImage[] = useMemo(() => (igdb.images ?? []).map((image, index) => ({
    // Stable while the picture switches from the remote URL to its cached copy.
    key: 'igdb:' + image.kind + ':' + index, thumb: image.full, full: image.full, igdb: true,
    alt: image.kind === 'artwork' ? 'Artwork from IGDB' : 'Screenshot from IGDB'
  })), [igdb.images])
  const both = yours.length > 0 && fromIgdb.length > 0
  const chosen = filter === 'yours' && both ? yours : filter === 'igdb' && both ? fromIgdb : [...yours, ...fromIgdb]
  const all = chosen.filter(image => !failed.has(image.key))
  const visible = showAll ? all : all.slice(0, FIRST_TILES)
  const columns = wallColumns(width)
  const placed = layoutWall(visible.map(image => ({ key: image.key, aspect: aspects[image.key] ?? image.aspect })), columns)
  // Reading order of the tiles: the lightbox and keyboard focus follow what the user sees.
  const order = visualOrder(placed)
  const ordered = [...order.map(index => visible[index]), ...all.slice(visible.length)]
  const cell = width > 0 ? Math.round(((width - (columns - 1) * 7) / columns) * 9 / 16) : 96
  const onAspect = useCallback((key: string, aspect: number) => setAspects(current => current[key] && Math.abs(current[key] - aspect) < 0.01 ? current : { ...current, [key]: aspect }), [])
  const lightboxImages: LightboxImage[] = ordered.map(image => ({ full: image.full, alt: image.alt, igdb: image.igdb }))
  const closeLightbox = useCallback(() => setOpen(null), [])
  const steam = isSteamGame(game)

  const add = async (fromSteam: boolean) => {
    setBusy(true)
    try {
      const result = fromSteam ? await window.lobby.screenshots.importSteam(game.id) : await window.lobby.screenshots.add(game.id)
      onShots(result.screenshots)
      const parts = [result.added ? 'Added ' + result.added + ' screenshot' + (result.added === 1 ? '' : 's') : '', result.restored ? 'Restored ' + result.restored + ' missing file' + (result.restored === 1 ? '' : 's') : '', result.duplicates ? result.duplicates + ' already added' : '', result.errors[0] ?? '']
      const text = parts.filter(Boolean).join(' · ')
      if (text) say(text + (result.errors.length > 1 ? ' (+' + (result.errors.length - 1) + ' more)' : ''))
    } catch (error) { say(error instanceof Error ? error.message : 'Could not add screenshots') }
    finally { setBusy(false) }
  }
  const setHero = async (shot: Screenshot) => {
    setMenu(null)
    try { await window.lobby.screenshots.setAsHero(shot.id); say('Screenshot set as the page background') }
    catch (error) { say(error instanceof Error ? error.message : 'Could not set the page background') }
  }
  const remove = async (shot: Screenshot) => {
    setConfirm(null)
    try { onShots(await window.lobby.screenshots.remove(shot.id)); setOpen(null) }
    catch (error) { say(error instanceof Error ? error.message : 'Could not remove the screenshot') }
  }

  if (yours.length === 0 && fromIgdb.length === 0) {
    return <section className="gp-wall">
      <button type="button" className="wall-empty" onClick={() => void add(false)} disabled={busy}>
        <ImagePlus size={22} />
        <b>Add your screenshots</b>
        <small>PNG, JPEG, WebP, GIF or BMP. Copies are kept in Game Lobby; your files stay where they are.</small>
      </button>
      {steam && <button type="button" className="link-button wall-steam-link" onClick={() => void add(true)} disabled={busy}>Import from Steam</button>}
      {note && <div className="wall-note" role="status">{note}</div>}
    </section>
  }

  return <section className="gp-wall">
    <div className="wall-head">
      <h2>Screenshots</h2>
      {both && <div className="segmented wall-filter" role="group" aria-label="Show pictures">
        {(['all', 'yours', 'igdb'] as Filter[]).map(value => <button key={value} className={filter === value ? 'active' : ''} aria-pressed={filter === value} onClick={() => { setFilter(value); setShowAll(false) }}>{value === 'all' ? 'All' : value === 'yours' ? 'Yours' : 'IGDB'}</button>)}
      </div>}
      <span className="wall-spacer" />
      {steam && <button type="button" className="secondary-button wall-action" onClick={() => void add(true)} disabled={busy}>Import from Steam</button>}
      <button type="button" className="secondary-button wall-action" onClick={() => void add(false)} disabled={busy}><ImagePlus size={14} /> Add screenshots</button>
    </div>
    <div className="wall-grid" ref={setGrid} style={{ gridTemplateColumns: 'repeat(' + columns + ',minmax(0,1fr))', gridAutoRows: cell + 'px' }}>
      {order.map((index, position) => <Tile key={visible[index].key} image={visible[index]} place={placed[index]} onOpen={() => setOpen(position)} onAspect={onAspect} onFailed={onFailed}
        onMenu={visible[index].shot ? (x, y) => setMenu({ x, y, shot: visible[index].shot! }) : undefined} />)}
    </div>
    <div className="wall-foot">
      {all.length > FIRST_TILES && <button type="button" className="link-button" onClick={() => setShowAll(value => !value)}>{showAll ? 'Show fewer' : 'Show all (' + all.length + ')'}</button>}
      {note && <span className="wall-note" role="status">{note}</span>}
      {all.some(image => image.igdb) && <IgdbAttribution images />}
    </div>
    {menu && <>
      <div className="menu-dismiss gp-dismiss" onMouseDown={() => setMenu(null)} />
      <div className="context-menu wall-menu" role="menu" style={{ left: Math.min(menu.x, window.innerWidth - 220), top: Math.min(menu.y, window.innerHeight - 100) }}>
        <button role="menuitem" onClick={() => void setHero(menu.shot)}>Set as page background</button>
        <button role="menuitem" className="danger" onClick={() => { setConfirm(menu.shot); setMenu(null) }}>Remove</button>
      </div>
    </>}
    {confirm && <div className="modal-scrim"><div className="modal" role="alertdialog" aria-label="Remove screenshot">
      <div className="modal-head"><h2>Remove screenshot</h2><button type="button" aria-label="Close" onClick={() => setConfirm(null)}><X size={17} /></button></div>
      <p className="muted-text">This removes Game Lobby's copy of the screenshot. The original file on your PC is not changed.</p>
      <div className="modal-actions"><button className="secondary-button" onClick={() => setConfirm(null)}>Cancel</button><button className="primary-button" onClick={() => void remove(confirm)}>Remove</button></div>
    </div></div>}
    {open !== null && lightboxImages.length > 0 && <Lightbox images={lightboxImages} index={Math.min(open, lightboxImages.length - 1)} onIndex={setOpen} onClose={closeLightbox} />}
  </section>
}
