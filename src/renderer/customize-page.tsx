import { useCallback, useEffect, useRef, useState } from 'react'
import { ImagePlus, RotateCcw, Search, X } from 'lucide-react'
import { coverUrl } from './lobby-api'
import { LoadMore, useSgdbPages } from './cover-picker'
import { defaultGamePageCustom } from '../shared/defaults'
import { artPreview } from '../shared/preview'
import { logoPositions, logoScaleRange } from '../shared/types'
import { FadeImg } from './loading-image'
import { useKeysUi } from './api-keys'
import type { GamePageCustom, LogoPosition, SteamGridDbCover, WikiImage, WikiView } from '../shared/types'
import { ImageCredit } from './wiki-details'

export interface LogoLayout { position: LogoPosition; scale: number }

const positionLabels: Record<LogoPosition, string> = {
  'bottom-left': 'Bottom left',
  'top-center': 'Top center',
  center: 'Center',
  'bottom-center': 'Bottom center'
}
const SAVE_DELAY = 350

/**
 * Local (instant) logo layout for the live preview, with debounced saving to the backend.
 * `custom` is the last saved value; it is adopted only while no local edit is waiting to be saved.
 */
export function useLogoLayout(gameId: string, custom: GamePageCustom) {
  const [layout, setLayout] = useState<LogoLayout>({ position: custom.logoPosition, scale: custom.logoScale })
  const pending = useRef<LogoLayout | null>(null)
  const timer = useRef<number | undefined>(undefined)
  const idRef = useRef(gameId)
  idRef.current = gameId

  const flush = useCallback(() => {
    window.clearTimeout(timer.current)
    const next = pending.current
    if (!next) return
    pending.current = null
    window.lobby.art.setLogoLayout(idRef.current, next.position, next.scale).catch(() => {})
  }, [])

  useEffect(() => {
    if (!pending.current) setLayout({ position: custom.logoPosition, scale: custom.logoScale })
  }, [custom.logoPosition, custom.logoScale])
  useEffect(() => () => flush(), [gameId, flush])

  const change = useCallback((next: LogoLayout) => {
    const scale = Math.min(logoScaleRange.max, Math.max(logoScaleRange.min, Math.round(next.scale * 100) / 100))
    const value = { position: next.position, scale }
    setLayout(value)
    pending.current = value
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(flush, SAVE_DELAY)
  }, [flush])

  return { layout, change, flush }
}

function ArtTile({ item, label, animated, disabled, onApply }: { item: SteamGridDbCover; label: string; animated: boolean; disabled: boolean; onApply(cover: SteamGridDbCover): void }) {
  const [failed, setFailed] = useState(false)
  const preview = artPreview(item, animated)
  if (!preview) return null
  return <button type="button" className={'cz-tile' + (failed ? ' failed' : '')} disabled={disabled} onClick={() => onApply(item)} title={item.author ? 'By ' + item.author : undefined} aria-label={label}>
    {failed
      ? <span className="cz-tile-note" aria-hidden="true">Preview unavailable</span>
      : preview.kind === 'video'
        ? <video src={preview.src} muted loop autoPlay playsInline preload="auto" disablePictureInPicture aria-hidden="true" ref={video => { if (video) video.muted = true }} onError={() => setFailed(true)} />
        : <FadeImg src={coverUrl(preview.src)} loading="lazy" alt="" onError={() => setFailed(true)} />}
  </button>
}

/** Free Wikimedia Commons images for the game, each with its credit; choosing one replaces the current one. */
function WikiTiles({ images, kind, busy, onUse }: { images: WikiImage[]; kind: 'logo' | 'cover'; busy: boolean; onUse(image: WikiImage): void }) {
  if (images.length === 0) return null
  return <div className="cz-wiki">
    <div className="cz-label">Free {kind}s from Wikimedia Commons</div>
    <div className={'cz-grid ' + (kind === 'logo' ? 'logo' : 'cover') + (busy ? ' busy' : '')}>
      {images.map(image => <div key={image.src} className="cz-wiki-item">
        <button type="button" className="cz-tile" disabled={busy} onClick={() => onUse(image)} aria-label={'Use ' + image.file}>
          <FadeImg src={coverUrl(image.display)} loading="lazy" alt="" />
        </button>
        <ImageCredit image={image} />
        {image.lowTrust && <small className="cz-wiki-note">Low resolution ({image.originalWidth}×{image.originalHeight})</small>}
      </div>)}
    </div>
  </div>
}

function ArtSection({ kind, gameId, title, currentSrc, hasCustom, settingsKey, animated, wikiLogos = [] }: { kind: 'hero' | 'logo'; gameId: string; title: string; currentSrc?: string; hasCustom: boolean; settingsKey: string; animated: boolean; wikiLogos?: WikiImage[] }) {
  const pages = useSgdbPages(kind, gameId, settingsKey)
  const { openSheet } = useKeysUi()
  const { state, covers: items, reset: resetPages } = pages
  const [actionError, setActionError] = useState('')
  const error = actionError || pages.error
  const setError = setActionError
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const noun = kind === 'hero' ? 'hero' : 'logo'

  useEffect(() => { resetPages(); setActionError(''); setNotice('') }, [gameId, kind, resetPages])
  useEffect(() => () => resetPages(), [resetPages])

  const openPicker = async () => {
    setActionError(''); setNotice('')
    await pages.start()
  }

  const act = async (job: () => Promise<unknown>, done?: string) => {
    if (busy) return
    setBusy(true); setError(''); setNotice('')
    try { await job(); if (done) setNotice(done) }
    catch (e) { setError(e instanceof Error ? e.message : 'Something went wrong') }
    finally { setBusy(false) }
  }
  const art = window.lobby.art
  const apply = (cover: SteamGridDbCover) => act(() => kind === 'hero' ? art.setHero(gameId, cover.url) : art.setLogo(gameId, cover.url), 'Applied')
  const chooseFile = () => act(() => kind === 'hero' ? art.chooseHeroFile(gameId) : art.chooseLogoFile(gameId))
  const reset = () => act(() => kind === 'hero' ? art.setHero(gameId, null) : art.setLogo(gameId, null), 'Reset to default')

  return <div className="cz-section">
    <div className={'cz-preview ' + kind}>
      {currentSrc ? <img key={currentSrc} src={currentSrc} alt={'Current ' + noun} onError={event => { event.currentTarget.style.display = 'none' }} /> : <span>No {noun} available. The game title is shown instead.</span>}
    </div>
    <div className="cz-row">
      <button type="button" className="secondary-button" onClick={() => void openPicker()} disabled={busy || state === 'loading'}><Search size={13} /> Choose from SteamGridDB</button>
      <button type="button" className="secondary-button" onClick={() => void chooseFile()} disabled={busy}><ImagePlus size={13} /> Choose image file…</button>
      <button type="button" className="secondary-button" onClick={() => void reset()} disabled={busy || !hasCustom} title={hasCustom ? undefined : 'Already using the automatic ' + noun}><RotateCcw size={13} /> Reset to default</button>
    </div>
    <div className="cz-status" role="status" aria-live="polite">{error ? <span className="cz-error">{error}</span> : notice}</div>
    {kind === 'logo' && <WikiTiles images={wikiLogos} kind="logo" busy={busy} onUse={image => void act(() => window.lobby.wikimedia.useLogo(gameId, image.src), 'Applied')} />}
    {state === 'loading' && <div className="cz-empty">Searching SteamGridDB…</div>}
    {state === 'nokey' && <div className="cz-empty">Add a free SteamGridDB API key to browse {noun}s. Choosing a file and resetting still work.<button type="button" className="secondary-button" onClick={() => openSheet('steamgriddb')}>Add API key</button></div>}
    {state === 'error' && <div className="cz-empty">{error || 'Could not search SteamGridDB'}<button type="button" className="secondary-button" onClick={() => void openPicker()}>Try again</button></div>}
    {state === 'ready' && items.length === 0 && !pages.hasMore && <div className="cz-empty">No {noun}s found on SteamGridDB for this game</div>}
    {state === 'ready' && items.length > 0 && <div className={'cz-grid ' + kind + (busy ? ' busy' : '')} aria-label={title + ' options'}>
      {items.map(item => <ArtTile key={item.id} item={item} label={item.author ? title + ' option by ' + item.author : title + ' option ' + item.id} animated={animated} disabled={busy} onApply={cover => void apply(cover)} />)}
    </div>}
    {state === 'ready' && (items.length > 0 || pages.hasMore) && <LoadMore shown={items.length} hasMore={pages.hasMore} loading={pages.loadingMore} onMore={() => void pages.loadMore()} noun={noun} />}
  </div>
}

type Tab = 'hero' | 'logo' | 'cover' | 'layout'

/** Free Commons covers for the library poster (only shown when the lookup found any). */
function CoverSection({ gameId, covers }: { gameId: string; covers: WikiImage[] }) {
  const [busy, setBusy] = useState(false)
  const [status, setStatus] = useState('')
  const use = async (image: WikiImage) => {
    setBusy(true); setStatus('')
    try { await window.lobby.wikimedia.useCover(gameId, image.src); setStatus('Cover applied') }
    catch (e) { setStatus(e instanceof Error ? e.message : typeof e === 'string' ? e : 'Could not use that cover') }
    finally { setBusy(false) }
  }
  return <div className="cz-section">
    <p className="cz-hint">These covers are free files from Wikimedia Commons. Choosing one replaces the game's current cover; the credit is shown under the poster.</p>
    <div className="cz-status" role="status" aria-live="polite">{status}</div>
    <WikiTiles images={covers} kind="cover" busy={busy} onUse={image => void use(image)} />
  </div>
}

export function CustomizePanel({ game, custom, heroSrc, logoSrc, layout, onLayout, onClose, sgdbSettingsKey = '', animated = false, wiki }: {
  game: { id: string; title: string }
  /** The game's Wikimedia lookup: free logos and covers offered as candidates. */
  wiki?: WikiView
  /** The SteamGridDB filters in effect (keys the session cache of picker pages). */
  sgdbSettingsKey?: string
  animated?: boolean
  custom: GamePageCustom
  heroSrc?: string
  logoSrc?: string
  layout: LogoLayout
  onLayout(layout: LogoLayout): void
  onClose(): void
}) {
  const [tab, setTab] = useState<Tab>('hero')
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    panelRef.current?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus()
    // Capture phase so the page-level Escape (back to library) does not also fire.
    const key = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return
      event.stopImmediatePropagation(); event.preventDefault()
      onClose()
    }
    window.addEventListener('keydown', key, true)
    return () => window.removeEventListener('keydown', key, true)
  }, [onClose])

  const wikiCovers = wiki?.enabled ? wiki.covers : []
  const tabs: Array<[Tab, string]> = [['hero', 'Hero'], ['logo', 'Logo'], ...(wikiCovers.length ? [['cover', 'Cover'] as [Tab, string]] : []), ['layout', 'Logo layout']]
  const onTabKey = (event: React.KeyboardEvent) => {
    const i = tabs.findIndex(([id]) => id === tab)
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0
    if (!step) return
    event.preventDefault()
    const next = tabs[(i + step + tabs.length) % tabs.length][0]
    setTab(next)
    window.requestAnimationFrame(() => panelRef.current?.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus())
  }
  const isDefaultLayout = layout.position === defaultGamePageCustom.logoPosition && layout.scale === defaultGamePageCustom.logoScale

  return <aside className="cz-panel" role="dialog" aria-label={'Customize ' + game.title} ref={panelRef}>
    <div className="cz-head"><h2>Customize page</h2><button type="button" className="cz-close" aria-label="Close" onClick={onClose}><X size={16} /></button></div>
    <div className="cz-tabs" role="tablist" onKeyDown={onTabKey}>
      {tabs.map(([id, label]) => <button key={id} type="button" role="tab" id={'cz-tab-' + id} aria-selected={tab === id} aria-controls="cz-body" tabIndex={tab === id ? 0 : -1} className={tab === id ? 'active' : ''} onClick={() => setTab(id)}>{label}</button>)}
    </div>
    <div className="cz-body" id="cz-body" role="tabpanel" aria-labelledby={'cz-tab-' + tab}>
      {tab === 'hero' && <ArtSection kind="hero" gameId={game.id} title="Hero" currentSrc={heroSrc} hasCustom={!!custom.hero} settingsKey={sgdbSettingsKey} animated={animated} />}
      {tab === 'logo' && <ArtSection kind="logo" gameId={game.id} title="Logo" currentSrc={logoSrc} hasCustom={!!custom.logo} settingsKey={sgdbSettingsKey} animated={animated} wikiLogos={wiki?.enabled ? wiki.logos : []} />}
      {tab === 'cover' && <CoverSection gameId={game.id} covers={wikiCovers} />}
      {tab === 'layout' && <div className="cz-section">
        <p className="cz-hint">Changes preview live on the page and save automatically.{!logoSrc && ' This game has no logo, so the title text is positioned instead.'}</p>
        <div className="cz-label">Position</div>
        <div className="cz-positions" role="radiogroup" aria-label="Logo position">
          {logoPositions.map(position => <button key={position} type="button" role="radio" aria-checked={layout.position === position} className={'cz-pos' + (layout.position === position ? ' active' : '')} onClick={() => onLayout({ ...layout, position })}>
            <span className={'cz-pos-icon ' + position} aria-hidden="true"><i /></span>{positionLabels[position]}
          </button>)}
        </div>
        <label className="cz-label" htmlFor="cz-scale">Size <b>{layout.scale.toFixed(2).replace(/0$/, '')}×</b></label>
        <input id="cz-scale" className="cz-slider" type="range" min={logoScaleRange.min} max={logoScaleRange.max} step={0.05} value={layout.scale}
          aria-valuetext={layout.scale.toFixed(2) + ' times'} onChange={event => onLayout({ ...layout, scale: Number(event.target.value) })} />
        <div className="cz-scale-ends"><span>{logoScaleRange.min}×</span><span>{logoScaleRange.max}×</span></div>
        <div className="cz-row"><button type="button" className="secondary-button" disabled={isDefaultLayout} onClick={() => onLayout({ position: defaultGamePageCustom.logoPosition, scale: defaultGamePageCustom.logoScale })}><RotateCcw size={13} /> Reset layout</button></div>
      </div>}
    </div>
  </aside>
}
