import { useState } from 'react'
import type { SteamGridDbCover } from '../shared/types'

/** How a candidate's preview is shown: an image, a muted looping video (animated grids), or not at all. */
export function previewKind(thumb: string, animated: boolean): 'image' | 'video' | null {
  let path: string
  try { path = new URL(thumb).pathname.toLowerCase() } catch { return null }
  if (path.endsWith('.webm') || path.endsWith('.mp4')) return animated && path.endsWith('.webm') ? 'video' : null
  return 'image'
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
    {state === 'loading' && <span className="sgdb-skeleton" aria-hidden="true" />}
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
