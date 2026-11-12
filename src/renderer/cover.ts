import type { CSSProperties } from 'react'
import type { Game } from '../shared/types'
import { coverUrl } from './lobby-api'

export const colors = ['#224d67', '#563339', '#32513d', '#544433', '#34394e', '#6c3c2b']

export function coverImage(value?: string) {
  if (!value) return undefined
  try {
    const url = new URL(coverUrl(value), window.location.href)
    if (url.protocol !== 'https:' && url.origin !== 'http://lobby-cover.localhost') return undefined
    const escaped = url.href.replace(/[\\"\r\n\f]/g, character => `\\${character}`)
    return `url("${escaped}")`
  } catch { return undefined }
}

export function coverStyle(game: Game, index: number): CSSProperties {
  const color = colors[index % colors.length]
  const image = coverImage(game.cover)
  // Longhands only: the `background` shorthand would reset background-size/position
  // and show the cover at its natural size, cropped to the top-left corner.
  return {
    backgroundImage: image
      ? `linear-gradient(0deg,rgba(0,0,0,.22),transparent 50%),${image}`
      : `linear-gradient(155deg, ${color}, #161719)`,
    backgroundSize: 'cover',
    backgroundPosition: 'center',
    backgroundRepeat: 'no-repeat'
  }
}
