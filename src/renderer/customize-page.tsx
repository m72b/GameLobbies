import { useCallback, useEffect, useRef, useState } from 'react'
import { ImagePlus, RotateCcw, Search, X } from 'lucide-react'
import { coverUrl } from './lobby-api'
import { defaultGamePageCustom } from '../shared/defaults'
import { logoPositions, logoScaleRange } from '../shared/types'
import type { GamePageCustom, LogoPosition, SteamGridDbCover } from '../shared/types'

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

type PickerState = 'idle' | 'loading' | 'ready' | 'nokey' | 'error'

function ArtSection({ kind, gameId, title, currentSrc, hasCustom }: { kind: 'hero' | 'logo'; gameId: string; title: string; currentSrc?: string; hasCustom: boolean }) {
  const [state, setState] = useState<PickerState>('idle')
  const [items, setItems] = useState<SteamGridDbCover[]>([])
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const run = useRef(0)
  const noun = kind === 'hero' ? 'hero' : 'logo'

  useEffect(() => { run.current++; setState('idle'); setItems([]); setError(''); setNotice('') }, [gameId, kind])
  useEffect(() => () => { run.current++ }, [])

  const openPicker = async () => {
    const id = ++run.current
    setState('loading'); setError(''); setNotice('')
    try {
      const { keySet } = await window.lobby.steamGridDb.status()
      if (id !== run.current) return
      if (!keySet) { setState('nokey'); return }
      const list = await (kind === 'hero' ? window.lobby.steamGridDb.searchHeroes(gameId) : window.lobby.steamGridDb.searchLogos(gameId))
      if (id !== run.current) return
      setItems(list); setState('ready')
    } catch (e) {
      if (id !== run.current) return
      setError(e instanceof Error ? e.message : 'Could not search SteamGridDB'); setState('error')
    }
  }

  const act = async (job: () => Promise<unknown>, done?: string) => {
    if (busy) return
    setBusy(true); setError(''); setNotice('')
    try { await job(); if (done) setNotice(done) }
    catch (e) { setError(e instanceof Error ? e.message : 'Something went wrong'); setState(value => value === 'error' ? 'idle' : value) }
    finally { setBusy(false) }
  }
  const art = window.lobby.art
  const apply = (cover: SteamGridDbCover) => act(() => kind === 'hero' ? art.setHero(gameId, cover.url) : art.setLogo(gameId, cover.url), 'Applied')
  const chooseFile = () => act(() => kind === 'hero' ? art.chooseHeroFile(gameId) : art.chooseLogoFile(gameId))
  const reset = () => act(() => kind === 'hero' ? art.setHero(gameId, null) : art.setLogo(gameId, null), 'Reset to default')

  return <div className="cz-section">
    <div className={'cz-preview ' + kind}>
      {currentSrc ? <img src={currentSrc} alt={'Current ' + noun} /> : <span>No {noun} available. The game title is shown instead.</span>}
    </div>
    <div className="cz-row">
      <button type="button" className="secondary-button" onClick={() => void openPicker()} disabled={busy || state === 'loading'}><Search size={13} /> Choose from SteamGridDB</button>
      <button type="button" className="secondary-button" onClick={() => void chooseFile()} disabled={busy}><ImagePlus size={13} /> Choose image file…</button>
      <button type="button" className="secondary-button" onClick={() => void reset()} disabled={busy || !hasCustom} title={hasCustom ? undefined : 'Already using the automatic ' + noun}><RotateCcw size={13} /> Reset to default</button>
    </div>
    <div className="cz-status" role="status" aria-live="polite">{error ? <span className="cz-error">{error}</span> : notice}</div>
    {state === 'loading' && <div className="cz-empty">Searching SteamGridDB…</div>}
    {state === 'nokey' && <div className="cz-empty">Add a free SteamGridDB API key in settings to browse {noun}s. Choosing a file and resetting still work.<button type="button" className="secondary-button" onClick={() => void window.lobby.settings.open('cover-art')}>Open settings</button></div>}
    {state === 'error' && <div className="cz-empty">{error || 'Could not search SteamGridDB'}<button type="button" className="secondary-button" onClick={() => void openPicker()}>Try again</button></div>}
    {state === 'ready' && items.length === 0 && <div className="cz-empty">No {noun}s found on SteamGridDB for this game</div>}
    {state === 'ready' && items.length > 0 && <div className={'cz-grid ' + kind + (busy ? ' busy' : '')} aria-label={title + ' options'}>
      {items.map(item => <button type="button" key={item.id} className="cz-tile" disabled={busy} onClick={() => void apply(item)} title={item.author ? 'By ' + item.author : undefined}>
        <img src={coverUrl(item.thumb || item.url)} loading="lazy" alt={item.author ? title + ' option by ' + item.author : title + ' option ' + item.id} />
      </button>)}
    </div>}
  </div>
}

type Tab = 'hero' | 'logo' | 'layout'

export function CustomizePanel({ game, custom, heroSrc, logoSrc, layout, onLayout, onClose }: {
  game: { id: string; title: string }
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

  const tabs: Array<[Tab, string]> = [['hero', 'Hero'], ['logo', 'Logo'], ['layout', 'Logo layout']]
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
      {tab === 'hero' && <ArtSection kind="hero" gameId={game.id} title="Hero" currentSrc={heroSrc} hasCustom={!!custom.hero} />}
      {tab === 'logo' && <ArtSection kind="logo" gameId={game.id} title="Logo" currentSrc={logoSrc} hasCustom={!!custom.logo} />}
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
