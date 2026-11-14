import { useCallback, useSyncExternalStore, type CSSProperties } from 'react'
import type { Game } from '../shared/types'
import { coverUrl } from './lobby-api'

export const colors = ['#224d67', '#563339', '#32513d', '#544433', '#34394e', '#6c3c2b']

/** The displayable URL of a stored cover: https, or a local file on the lobby-cover origin. */
export function coverSrc(value?: string): string | undefined {
  if (!value) return undefined
  try {
    const url = new URL(coverUrl(value), window.location.href)
    if (url.protocol !== 'https:' && url.origin !== 'http://lobby-cover.localhost' && url.origin !== 'http://lobby-shot.localhost') return undefined
    return url.href
  } catch { return undefined }
}

/** `url("…")` for a URL from {@link coverSrc}, escaped for CSS. */
export function cssUrl(src: string) {
  return `url("${src.replace(/[\\"\r\n\f]/g, character => `\\${character}`)}")`
}

/** The colored placeholder behind every poster; shown on its own while the image loads or when it fails. */
export function coverStyle(game: Game, index: number): CSSProperties {
  const color = colors[index % colors.length]
  // Longhands only: the `background` shorthand would reset background-size/position.
  return {
    backgroundImage: `linear-gradient(155deg, ${color}, #161719)`,
    backgroundSize: 'cover',
    backgroundPosition: 'center',
    backgroundRepeat: 'no-repeat'
  }
}

/** Style for the image layer drawn over {@link coverStyle} once the image has loaded. */
export function coverLayerStyle(src: string): CSSProperties {
  return {
    backgroundImage: `linear-gradient(0deg,rgba(0,0,0,.22),transparent 50%),${cssUrl(src)}`,
    backgroundSize: 'cover',
    backgroundPosition: 'center',
    backgroundRepeat: 'no-repeat'
  }
}

// ---------------------------------------------------------------------------
// Load state per URL, shared by every card for the whole session: a URL is requested once
// (re-renders and remounts while scrolling reuse the result), a new URL (e.g. the local cached
// file replacing a remote one) starts a fresh load, and a failed URL is retried after a while.

export type CoverStatus = 'none' | 'loading' | 'loaded' | 'error'

const RETRY_AFTER_MS = 30_000
/** Loaded images kept referenced so the CSS background is served from memory. */
const MAX_RETAINED = 300

const states = new Map<string, { status: 'loading' | 'loaded' | 'error'; at: number }>()
const listeners = new Map<string, Set<() => void>>()
const retained = new Map<string, HTMLImageElement>()
const retryTimers = new Map<string, number>()

function setStatus(src: string, status: 'loading' | 'loaded' | 'error') {
  states.set(src, { status, at: Date.now() })
  listeners.get(src)?.forEach(listener => listener())
}

function load(src: string) {
  const state = states.get(src)
  if (state && (state.status !== 'error' || Date.now() - state.at < RETRY_AFTER_MS)) return
  setStatus(src, 'loading')
  const image = new Image()
  image.decoding = 'async'
  image.onload = () => {
    retained.delete(src)
    retained.set(src, image)
    if (retained.size > MAX_RETAINED) retained.delete(retained.keys().next().value!)
    setStatus(src, 'loaded')
  }
  image.onerror = () => {
    setStatus(src, 'error')
    // Still on screen: try once more later (a remote CDN hiccup should not stick).
    if (!retryTimers.has(src)) {
      retryTimers.set(src, window.setTimeout(() => {
        retryTimers.delete(src)
        if (listeners.get(src)?.size) load(src)
      }, RETRY_AFTER_MS))
    }
  }
  image.src = src
}

/** The load state of a cover URL; starts loading it when first used. */
export function useCoverStatus(src?: string): CoverStatus {
  const subscribe = useCallback((listener: () => void) => {
    if (!src) return () => {}
    let set = listeners.get(src)
    if (!set) listeners.set(src, set = new Set())
    set.add(listener)
    load(src)
    return () => {
      set.delete(listener)
      if (!set.size) listeners.delete(src)
    }
  }, [src])
  return useSyncExternalStore(subscribe, () => src ? states.get(src)?.status ?? 'loading' : 'none')
}

/** Whether the URL finished loading just now (fade it in; older loads show at once on remount). */
export function justLoaded(src: string) {
  const state = states.get(src)
  return state?.status === 'loaded' && Date.now() - state.at < 600
}
