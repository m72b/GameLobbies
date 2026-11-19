import { ghostFrame, rectOf, type Rect } from '../shared/motion'
import { EASE, currentMotion, durationFor, play } from './motion'
import { cssUrl } from './cover'

/** Where a game page opens from: the clicked poster's box on screen and its cover. */
export interface ZoomOrigin { rect: Rect; src?: string; radius: number }

const ZOOM_MS = 320
const FADE_MS = 140

/** The soft spring from the CSS tokens (a `linear()` curve where supported), so JS and CSS agree. */
export function springSoft(): string {
  return getComputedStyle(document.documentElement).getPropertyValue('--spring-soft').trim() || EASE
}

/** The poster inside a clicked card (or the element itself), as a zoom origin. */
export function originOf(card: Element | null | undefined, src: string | undefined): ZoomOrigin | null {
  const poster = card?.classList.contains('poster') ? card : card?.querySelector('.poster')
  if (!poster) return null
  const box = poster.getBoundingClientRect()
  if (!(box.width > 0) || !(box.height > 0)) return null
  const radius = Number.parseFloat(getComputedStyle(poster).borderTopLeftRadius) || 10
  return { rect: rectOf(box), src, radius }
}

function makeGhost(rect: Rect, src: string | undefined, radius: number): HTMLElement {
  const ghost = document.createElement('div')
  ghost.className = 'zoom-ghost'
  ghost.setAttribute('aria-hidden', 'true')
  if (src) ghost.style.backgroundImage = 'linear-gradient(0deg,rgba(0,0,0,.22),transparent 50%),' + cssUrl(src)
  else ghost.style.backgroundColor = 'var(--surface-2)'
  Object.assign(ghost.style, ghostFrame(rect, radius))
  document.body.appendChild(ghost)
  return ghost
}

/**
 * The game page opens. With a poster to come from (full motion) the cover grows from the poster
 * to the hero (a "ghost" box, so the art is cropped rather than stretched) while the hero fades
 * in under it, and everything below the hero follows a moment later. Without a poster: a gentle
 * scale and fade. Reduced: a short fade. Off: nothing. Returns a cleanup that stops it all.
 */
export function zoomIn(page: HTMLElement, origin: ZoomOrigin | null): () => void {
  const level = currentMotion()
  const running: Animation[] = []
  let ghost: HTMLElement | null = null
  const start = (element: Element, frames: Keyframe[], ms: number, options?: KeyframeAnimationOptions) => {
    const animation = play(element, frames, ms, options)
    if (animation) running.push(animation)
    return animation
  }
  const stop = () => { running.forEach(animation => animation.cancel()); ghost?.remove(); ghost = null }
  if (level === 'off') return stop
  if (level === 'reduced') { start(page, [{ opacity: 0 }, { opacity: 1 }], durationFor(ZOOM_MS, level)); return stop }
  const hero = page.querySelector<HTMLElement>('.gp-hero')
  if (!origin || !hero) {
    start(page, [{ opacity: 0, transform: 'scale(.985)' }, { opacity: 1, transform: 'none' }], 260, { easing: springSoft() })
    return stop
  }
  const target = rectOf(hero.getBoundingClientRect())
  ghost = makeGhost(origin.rect, origin.src, origin.radius)
  const made = ghost
  start(made, [ghostFrame(origin.rect, origin.radius), ghostFrame(target, 0)], ZOOM_MS, { easing: springSoft(), fill: 'forwards' })
  const fade = start(made, [{ opacity: 1 }, { opacity: 0 }], 140, { delay: ZOOM_MS * 0.72, fill: 'forwards' })
  fade?.finished.then(() => made.remove(), () => undefined)
  start(hero, [{ opacity: 0 }, { opacity: 1 }], 220, { delay: 40, fill: 'backwards' })
  for (const part of Array.from(page.children)) {
    if (part !== hero) start(part, [{ opacity: 0, transform: 'translateY(10px)' }, { opacity: 1, transform: 'none' }], 260, { delay: 150, fill: 'backwards' })
  }
  return stop
}

/**
 * The game page closes (the library is already showing). With the poster still on screen
 * (`target`) the hero's art shrinks back onto it; otherwise it fades and settles away.
 */
export function zoomOut(from: Rect, src: string | undefined, target: { rect: Rect; radius: number } | null): void {
  const level = currentMotion()
  if (level !== 'full') return
  const ghost = makeGhost(from, src, 0)
  const done = () => ghost.remove()
  if (target) {
    play(ghost, [ghostFrame(from, 0), ghostFrame(target.rect, target.radius)], ZOOM_MS - 40, { easing: springSoft(), fill: 'forwards' })
    const fade = play(ghost, [{ opacity: 1 }, { opacity: 0 }], 150, { delay: (ZOOM_MS - 40) * 0.6, fill: 'forwards' })
    if (fade) fade.finished.then(done, done); else done()
  } else {
    const fade = play(ghost, [{ opacity: 1, transform: 'scale(1)' }, { opacity: 0, transform: 'scale(.98)' }], 180, { fill: 'forwards' })
    if (fade) fade.finished.then(done, done); else done()
  }
}

/** Fades the given elements in (a short, plain ease-out), e.g. the library as the page leaves. */
export function fadeIn(elements: Element[], ms = FADE_MS) {
  const level = currentMotion()
  for (const element of elements) play(element, [{ opacity: 0 }, { opacity: 1 }], durationFor(ms, level))
}
