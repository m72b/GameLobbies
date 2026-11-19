// Pure helpers for the app's transitions (zoom to the game page, grid fades, sheets).
// Tested by motion.test.mjs (`npm run test:web`; Node strips the TypeScript types).

/** Fades and small moves: ease-out, no overshoot. */
export const EASE = 'cubic-bezier(.2,.8,.2,1)'
/** Things that travel (the game page zoom): a smooth ease-in-out, no overshoot. Same as `--ease-move`. */
export const EASE_MOVE = 'cubic-bezier(.65,0,.35,1)'

export interface Rect { left: number; top: number; width: number; height: number }

/** How much motion the user wants: everything, short fades only, or none. */
export type MotionPreferenceValue = 'system' | 'full' | 'reduced' | 'off'
export type MotionLevel = 'full' | 'reduced' | 'off'

/** Duration of the short fade used instead of movement when motion is reduced. */
export const REDUCED_MS = 80
/** The game page's fade when motion is reduced. */
export const REDUCED_PAGE_MS = 120

/**
 * A transition's duration at the given level: full length normally, a short fade with reduced
 * motion (`prefers-reduced-motion`), and none when animations are off in Settings.
 */
export function durationFor(ms: number, level: MotionLevel): number {
  if (level === 'off') return 0
  if (level === 'reduced') return Math.min(ms, REDUCED_MS)
  return Math.max(0, ms)
}

/**
 * The motion level in effect: the Settings choice and the older "Animations" switch (off stops
 * everything). "full" always animates, whatever Windows prefers; "system" follows the OS.
 */
export function motionLevel(prefersReduced: boolean, preference: MotionPreferenceValue, animationsOn = true): MotionLevel {
  if (preference === 'off' || !animationsOn) return 'off'
  if (preference === 'full') return 'full'
  if (preference === 'reduced') return 'reduced'
  return prefersReduced ? 'reduced' : 'full'
}

/** Reads a `data-motion` attribute value; anything unknown is full motion. */
export const parseMotion = (value: string | undefined): MotionLevel => value === 'reduced' || value === 'off' ? value : 'full'

/** Timing of the game page opening (ms, from the moment the cover is ready). */
export const OPEN = {
  liftMs: 90, liftScale: 1.04, liftSettleMs: 150,
  zoomDelayMs: 60, zoomMs: 450,
  heroFadeDelayMs: 200, heroFadeMs: 240,
  coverFadeDelayMs: 330, coverFadeMs: 180,
  cascadeStartMs: 260, cascadeStaggerMs: 60, cascadeMs: 300, cascadeRisePx: 12,
  dimMs: 160, dimTo: 0.4, recedeScale: 0.98, recedeMs: 360, backdropFadeDelayMs: 120, backdropFadeMs: 330,
  noCardMs: 300, noCardScale: 0.96, decodeCapMs: 120
} as const

/** Timing of the game page closing. */
export const CLOSE = { contentMs: 120, shrinkDelayMs: 70, shrinkMs: 340, coverFadeMs: 140, undimMs: 340, undimFrom: 0.55, centerMs: 300, centerScale: 0.5 } as const

/** Start delays for a cascade of `count` items. */
export const cascadeDelays = (count: number, startMs: number, staggerMs: number): number[] =>
  Array.from({ length: Math.max(0, count) }, (_, index) => startMs + index * staggerMs)

/** When the whole opening sequence has finished, given how many items cascade in. */
export const openTotalMs = (cascadeCount: number): number => Math.max(
  OPEN.zoomDelayMs + OPEN.zoomMs,
  OPEN.cascadeStartMs + Math.max(0, cascadeCount - 1) * OPEN.cascadeStaggerMs + OPEN.cascadeMs
)

export const rectOf = (box: Rect): Rect => ({ left: box.left, top: box.top, width: box.width, height: box.height })

/** Whether a rect has a usable size and lies mostly inside the viewport/container `within`. */
export function isRectVisible(rect: Rect, within: Rect, minShare = 0.6): boolean {
  if (!(rect.width > 0) || !(rect.height > 0)) return false
  const overlapW = Math.min(rect.left + rect.width, within.left + within.width) - Math.max(rect.left, within.left)
  const overlapH = Math.min(rect.top + rect.height, within.top + within.height) - Math.max(rect.top, within.top)
  if (overlapW <= 0 || overlapH <= 0) return false
  return (overlapW * overlapH) / (rect.width * rect.height) >= minShare
}

/** The centre of a rect. */
export const centerOf = (rect: Rect) => ({ x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 })

/**
 * The transform (about the element's centre) that makes an element laid out at `from` cover `to`
 * without distortion: one uniform scale, centred on `to`.
 */
export function coverTransform(from: Rect, to: Rect): { x: number; y: number; scale: number } {
  const scale = Math.max(to.width / from.width, to.height / from.height)
  const a = centerOf(from)
  const b = centerOf(to)
  return { x: b.x - a.x, y: b.y - a.y, scale: Number.isFinite(scale) && scale > 0 ? scale : 1 }
}

export const transformCss = (t: { x: number; y: number; scale: number }) => 'translate(' + t.x + 'px,' + t.y + 'px) scale(' + t.scale + ')'

/** `clip-path` that shows only `rect` of a layer sized `width` x `height` (rect in the layer's coordinates), with rounded corners. */
export function insetClip(rect: Rect, width: number, height: number, radius: number): string {
  const top = rect.top, left = rect.left, right = width - (rect.left + rect.width), bottom = height - (rect.top + rect.height)
  return 'inset(' + top + 'px ' + right + 'px ' + bottom + 'px ' + left + 'px round ' + radius + 'px)'
}

/** `rect` moved into the coordinates of `origin` (its top-left becomes 0,0). */
export const relativeTo = (rect: Rect, origin: Rect): Rect => ({ left: rect.left - origin.left, top: rect.top - origin.top, width: rect.width, height: rect.height })

/** The grid's identity for transitions: only a change of this (not of the games) animates. */
export const gridKey = (view: string, store: string, onlyInstalled: boolean) => view + '\u0000' + store + '\u0000' + (onlyInstalled ? '1' : '0')
