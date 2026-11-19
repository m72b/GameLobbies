import { CLOSE, EASE_MOVE, OPEN, REDUCED_PAGE_MS, cascadeDelays, centerOf, coverTransform, insetClip, isRectVisible, rectOf, relativeTo, transformCss, type Rect } from '../shared/motion'
import { EASE, currentMotion } from './motion'
import { cssUrl } from './cover'

/** Where a game page opens from: the clicked poster's box on screen and its cover. */
export interface ZoomOrigin { rect: Rect; src?: string; radius: number }

/*
 * Technique: the cover grows with a "clip layer", not by resizing a box. A layer the size of the
 * main area holds one art element laid out once at its starting size; its transform (translate +
 * one uniform scale, GPU-composited) carries it, and `clip-path: inset(...round r)` on the layer
 * crops it from the poster's rect to the hero's rect. The art is never stretched (uniform scale),
 * nothing triggers layout, and the corners round smoothly. Every curve is overshoot-free.
 */

const live = new Set<HTMLElement>()
let backdrop: HTMLElement | null = null
let snapshot: { shell: HTMLElement; bounds: Rect } | null = null

function discard(element: HTMLElement | null | undefined) {
  if (!element) return
  element.remove()
  live.delete(element)
}

/** An overlay div fixed on `box`; removed on its own after a safety timeout. */
function overlay(className: string, box: Rect): HTMLElement {
  const element = document.createElement('div')
  element.className = className
  element.setAttribute('aria-hidden', 'true')
  element.style.cssText = 'left:' + box.left + 'px;top:' + box.top + 'px;width:' + box.width + 'px;height:' + box.height + 'px'
  document.body.appendChild(element)
  live.add(element)
  setTimeout(() => discard(element), 2500)
  return element
}

const animate = (element: Element, frames: Keyframe[], ms: number, options: KeyframeAnimationOptions = {}): Animation | null =>
  ms > 0 && typeof element.animate === 'function' ? element.animate(frames, { duration: ms, easing: EASE, fill: 'both', ...options }) : null

/** Removes `elements` once all `animations` have ended (or failed). */
function removeAfter(animations: Array<Animation | null>, ...elements: Array<HTMLElement | null>) {
  const done = () => elements.forEach(discard)
  const all = animations.filter((animation): animation is Animation => !!animation)
  if (!all.length) { done(); return }
  Promise.all(all.map(animation => animation.finished)).then(done, done)
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

/** Resolves when the image is decoded, or after `capMs` at the latest (the animation then starts anyway). */
function decoded(src: string | undefined, capMs: number): Promise<void> {
  if (!src) return Promise.resolve()
  return new Promise(resolve => {
    const timer = setTimeout(resolve, capMs)
    const done = () => { clearTimeout(timer); resolve() }
    const image = new Image()
    image.src = src
    if (typeof image.decode === 'function') image.decode().then(done, done)
    else { image.onload = done; image.onerror = done }
  })
}

/** Copies the scroll positions of `original`'s scrollable parts onto `clone` (a cloned subtree). */
function copyScroll(original: Element, clone: Element) {
  const from = [original, ...Array.from(original.querySelectorAll('*'))]
  const to = [clone, ...Array.from(clone.querySelectorAll('*'))]
  from.forEach((element, index) => { if (element.scrollTop || element.scrollLeft) { to[index].scrollTop = element.scrollTop; to[index].scrollLeft = element.scrollLeft } })
}

function cloneOf(source: HTMLElement): HTMLElement {
  const clone = source.cloneNode(true) as HTMLElement
  clone.querySelectorAll('[id]').forEach(element => element.removeAttribute('id'))
  clone.removeAttribute('id')
  clone.setAttribute('inert', '')
  clone.style.cssText += ';position:absolute;inset:0;width:100%;height:100%;margin:0'
  return clone
}

/**
 * Call just before a game page opens from the library: keeps a still copy of the library that
 * dims and recedes while the page comes in (the library itself is replaced by the page).
 */
export function captureLibrary(main: HTMLElement | null) {
  discard(backdrop)
  backdrop = null
  if (!main || currentMotion() !== 'full') return
  const box = main.getBoundingClientRect()
  if (!(box.width > 0) || !(box.height > 0)) return
  const shell = overlay('zoom-backdrop', rectOf(box))
  const inner = document.createElement('div')
  inner.className = 'zoom-backdrop-inner'
  const copy = cloneOf(main)
  inner.appendChild(copy)
  const scrim = document.createElement('div')
  scrim.className = 'zoom-scrim'
  inner.appendChild(scrim)
  shell.appendChild(inner)
  copyScroll(main, copy)
  backdrop = shell
}

/** A clip layer (see the note at the top): the pieces to animate. */
function clipLayer(bounds: Rect, base: Rect, src: string | undefined) {
  const root = overlay('zoom-layer', bounds)
  const lift = document.createElement('div')
  lift.className = 'zoom-lift'
  const clip = document.createElement('div')
  clip.className = 'zoom-clip'
  const art = document.createElement('div')
  art.className = 'zoom-art'
  if (src) art.style.backgroundImage = 'linear-gradient(0deg,rgba(0,0,0,.22),transparent 50%),' + cssUrl(src)
  else art.style.backgroundColor = 'var(--surface-2)'
  const place = relativeTo(base, bounds)
  art.style.cssText += ';left:' + place.left + 'px;top:' + place.top + 'px;width:' + place.width + 'px;height:' + place.height + 'px'
  clip.appendChild(art)
  lift.appendChild(clip)
  root.appendChild(lift)
  return { root, lift, clip, art }
}

/** Animates a clip layer so its window goes from `from` to `to` (screen rects) while the art covers it. */
function growClip(layer: ReturnType<typeof clipLayer>, bounds: Rect, base: Rect, from: Rect, to: Rect, radii: [number, number], ms: number, delay: number) {
  const rel = (rect: Rect, radius: number) => insetClip(relativeTo(rect, bounds), bounds.width, bounds.height, radius)
  layer.clip.style.clipPath = rel(from, radii[0])
  const timing = { delay, easing: EASE_MOVE }
  return [
    animate(layer.clip, [{ clipPath: rel(from, radii[0]) }, { clipPath: rel(to, radii[1]) }], ms, timing),
    animate(layer.art, [{ transform: 'translate(0px,0px) scale(1)' }, { transform: transformCss(coverTransform(base, to)) }], ms, timing)
  ]
}

/**
 * The game page opens. With a poster to come from (full motion): the poster lifts, its cover
 * grows to the hero while cross-fading into the hero art, the library behind dims and recedes,
 * and the bar, title and content cascade in. Without a poster: a gentle scale and fade.
 * Reduced: a short fade. Off: nothing. Returns a cleanup that stops it all.
 */
export function zoomIn(page: HTMLElement, origin: ZoomOrigin | null): () => void {
  const level = currentMotion()
  const taken = backdrop
  backdrop = null
  const created: HTMLElement[] = []
  const running: Animation[] = []
  let cancelled = false
  const track = (animation: Animation | null) => { if (animation) running.push(animation); return animation }
  const stop = () => {
    cancelled = true
    running.forEach(animation => animation.cancel())
    created.forEach(discard)
    discard(taken)
    page.style.opacity = ''
  }
  if (level === 'off') { discard(taken); return stop }
  if (level === 'reduced') { discard(taken); track(animate(page, [{ opacity: 0 }, { opacity: 1 }], REDUCED_PAGE_MS, { fill: 'backwards' })); return stop }
  const hero = page.querySelector<HTMLElement>('.gp-hero')
  if (!origin || !hero) {
    discard(taken)
    track(animate(page, [{ opacity: 0, transform: 'scale(' + OPEN.noCardScale + ')' }, { opacity: 1, transform: 'none' }], OPEN.noCardMs, { fill: 'backwards', easing: EASE_MOVE }))
    return stop
  }
  // Hold the page until the cover is decoded (a moment at most), then start everything together.
  page.style.opacity = '0'
  void decoded(origin.src, OPEN.decodeCapMs).then(() => {
    if (cancelled) return
    const bounds = rectOf((page.parentElement ?? page).getBoundingClientRect())
    const target = rectOf(hero.getBoundingClientRect())
    const layer = clipLayer(bounds, origin.rect, origin.src)
    created.push(layer.root)
    // Lift: the poster grows a little and a soft shadow fades in (an opacity'd element, not an animated box-shadow).
    const shadow = document.createElement('div')
    shadow.className = 'zoom-shadow'
    const place = relativeTo(origin.rect, bounds)
    shadow.style.cssText = 'left:' + place.left + 'px;top:' + place.top + 'px;width:' + place.width + 'px;height:' + place.height + 'px;border-radius:' + origin.radius + 'px'
    layer.root.insertBefore(shadow, layer.lift)
    const centre = centerOf(place)
    layer.lift.style.transformOrigin = centre.x + 'px ' + centre.y + 'px'
    const lifted = OPEN.liftMs + OPEN.liftSettleMs
    const peak = OPEN.liftMs / lifted
    const lift = [
      track(animate(layer.lift, [{ scale: 1 }, { scale: OPEN.liftScale, offset: peak }, { scale: 1 }], lifted)),
      track(animate(shadow, [{ opacity: 0 }, { opacity: 1, offset: peak }, { opacity: 0 }], lifted))
    ]
    const grow = growClip(layer, bounds, origin.rect, origin.rect, target, [origin.radius, 0], OPEN.zoomMs, OPEN.zoomDelayMs)
    grow.forEach(track)
    const coverFade = track(animate(layer.root, [{ opacity: 1 }, { opacity: 0 }], OPEN.coverFadeMs, { delay: OPEN.coverFadeDelayMs }))
    track(animate(hero, [{ opacity: 0 }, { opacity: 1 }], OPEN.heroFadeMs, { delay: OPEN.heroFadeDelayMs, fill: 'backwards' }))
    // The library behind: dims and recedes, then gives way to the page.
    if (taken) {
      const inner = taken.firstElementChild
      const scrim = inner?.querySelector('.zoom-scrim')
      if (inner && scrim) {
        created.push(taken)
        track(animate(scrim, [{ opacity: 0 }, { opacity: OPEN.dimTo }], OPEN.dimMs))
        track(animate(inner, [{ transform: 'none' }, { transform: 'scale(' + OPEN.recedeScale + ')' }], OPEN.recedeMs))
        const gone = track(animate(taken, [{ opacity: 1 }, { opacity: 0 }], OPEN.backdropFadeMs, { delay: OPEN.backdropFadeDelayMs }))
        removeAfter([gone], taken)
      } else discard(taken)
    }
    // Title/logo, play bar and content follow, one after another.
    const logo = hero.querySelector('.gp-logo-wrap')
    const bar = page.querySelector('.gp-bar')
    const body = page.querySelector('.gp-body')
    const parts = [logo, bar, ...(body ? Array.from(body.children) : [])].filter((part): part is Element => !!part)
    const delays = cascadeDelays(parts.length, OPEN.cascadeStartMs, OPEN.cascadeStaggerMs)
    parts.forEach((part, index) => track(animate(part, [{ opacity: 0, transform: 'translateY(' + OPEN.cascadeRisePx + 'px)' }, { opacity: 1, transform: 'none' }], OPEN.cascadeMs, { delay: delays[index], fill: 'backwards' })))
    page.style.opacity = ''
    removeAfter([coverFade, ...lift, ...grow], layer.root)
  })
  return stop
}

/** Call just before a game page closes: keeps a still copy of the page that fades out quickly. */
export function beginClose(page: HTMLElement | null) {
  discard(snapshot?.shell)
  snapshot = null
  if (!page || currentMotion() !== 'full') return
  const bounds = rectOf((page.parentElement ?? page).getBoundingClientRect())
  const shell = overlay('zoom-snap', bounds)
  const copy = cloneOf(page)
  shell.appendChild(copy)
  copyScroll(page, copy)
  snapshot = { shell, bounds }
  removeAfter([animate(shell, [{ opacity: 1 }, { opacity: 0 }], CLOSE.contentMs)], shell)
}

/**
 * The game page closes (the library is already showing; `library` are its top-level parts). With
 * the poster still on screen (`target`) the hero art shrinks back onto it; otherwise it scales
 * down and fades toward the grid's centre. The library un-dims.
 */
export function zoomOut(from: Rect, src: string | undefined, target: { rect: Rect; radius: number } | null, library: Element[], gridCenter?: { x: number; y: number }) {
  const snap = snapshot
  snapshot = null
  if (currentMotion() !== 'full' || !snap) return
  const bounds = snap.bounds
  if (!isRectVisible(from, bounds, 0.25)) return
  const layer = clipLayer(bounds, from, src)
  layer.root.classList.add('under')
  if (target) {
    const grow = growClip(layer, bounds, from, from, target.rect, [0, target.radius], CLOSE.shrinkMs, CLOSE.shrinkDelayMs)
    const fade = animate(layer.root, [{ opacity: 1 }, { opacity: 0 }], CLOSE.coverFadeMs, { delay: CLOSE.shrinkDelayMs + CLOSE.shrinkMs - CLOSE.coverFadeMs })
    removeAfter([...grow, fade], layer.root)
  } else {
    layer.clip.style.clipPath = insetClip(relativeTo(from, bounds), bounds.width, bounds.height, 0)
    const centre = centerOf(from)
    const goal = gridCenter ?? centerOf(bounds)
    const move = animate(layer.root, [{ opacity: 1, transform: 'none' }, { opacity: 0, transform: 'translate(' + (goal.x - centre.x) + 'px,' + (goal.y - centre.y) + 'px) scale(' + CLOSE.centerScale + ')' }], CLOSE.centerMs, { delay: CLOSE.shrinkDelayMs, easing: EASE_MOVE })
    removeAfter([move], layer.root)
  }
  for (const part of library) animate(part, [{ opacity: CLOSE.undimFrom }, { opacity: 1 }], CLOSE.undimMs, { fill: 'backwards' })
}

/** Fades the given elements in (a short, plain ease-out), e.g. the library as the page leaves. */
export function fadeIn(elements: Element[], ms = 140) {
  const level = currentMotion()
  const length = level === 'off' ? 0 : level === 'reduced' ? Math.min(ms, 80) : ms
  for (const element of elements) animate(element, [{ opacity: 0 }, { opacity: 1 }], length, { fill: 'none' })
}
