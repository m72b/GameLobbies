import type { Game } from '../shared/types'
import { Spinner } from './loading-image'
import { coverLayerStyle, coverSrc, justLoaded, useCoverStatus, type CoverStatus } from './cover'

/**
 * A cover's image layer over the colored placeholder ({@link coverStyle}): a shimmer while the
 * image loads, the image (faded in) once it has loaded, nothing when there is none or it failed.
 * `status` tells the caller whether to show the title fallback.
 */
export function useCover(game: Game): { src?: string; status: CoverStatus } {
  const src = coverSrc(game.cover)
  return { src, status: useCoverStatus(src) }
}

export function CoverLayer({ src, status }: { src?: string; status: CoverStatus }) {
  if (status === 'loading') return <div className="cover-shimmer" aria-hidden="true"><Spinner /></div>
  if (!src || status !== 'loaded') return null
  return <div className={'cover-layer' + (justLoaded(src) ? ' fade-in' : '')} style={coverLayerStyle(src)} aria-hidden="true" />
}
