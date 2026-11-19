// Pure helpers for the app's transitions (zoom to the game page, grid fades, sheets).
// Tested by motion.test.mjs (`npm run test:web`; Node strips the TypeScript types).

/** The one easing every transition uses (ease-out, a soft landing). */
export const EASE = 'cubic-bezier(.2,.8,.2,1)'

export interface Rect { left: number; top: number; width: number; height: number }

/** How much motion the user wants: everything, short fades only, or none. */
export type MotionLevel = 'full' | 'reduced' | 'off'

/** Duration of the short fade used instead of movement when motion is reduced. */
export const REDUCED_MS = 80

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
 * The motion level in effect: the Settings choice ("system" follows the Windows/OS preference)
 * and the older "Animations" switch, which when off also stops everything.
 */
export function motionLevel(prefersReduced: boolean, preference: 'system' | 'reduced' | 'off', animationsOn = true): MotionLevel {
  if (preference === 'off' || !animationsOn) return 'off'
  if (preference === 'reduced') return 'reduced'
  return prefersReduced ? 'reduced' : 'full'
}

/** Reads a `data-motion` attribute value; anything unknown is full motion. */
export const parseMotion = (value: string | undefined): MotionLevel => value === 'reduced' || value === 'off' ? value : 'full'


export const rectOf = (box: Rect): Rect => ({ left: box.left, top: box.top, width: box.width, height: box.height })

/** Whether a rect has a usable size and lies mostly inside the viewport/container `within`. */
export function isRectVisible(rect: Rect, within: Rect, minShare = 0.6): boolean {
  if (!(rect.width > 0) || !(rect.height > 0)) return false
  const overlapW = Math.min(rect.left + rect.width, within.left + within.width) - Math.max(rect.left, within.left)
  const overlapH = Math.min(rect.top + rect.height, within.top + within.height) - Math.max(rect.top, within.top)
  if (overlapW <= 0 || overlapH <= 0) return false
  return (overlapW * overlapH) / (rect.width * rect.height) >= minShare
}

/** The transform that makes an element laid out at `to` look like it sits at `from` (FLIP "invert"). */
export function flipTransform(from: Rect, to: Rect): { x: number; y: number; scaleX: number; scaleY: number } {
  const scaleX = to.width > 0 ? from.width / to.width : 1
  const scaleY = to.height > 0 ? from.height / to.height : 1
  return { x: from.left - to.left, y: from.top - to.top, scaleX, scaleY }
}

export const flipCss = (t: { x: number; y: number; scaleX: number; scaleY: number }) =>
  'translate(' + t.x + 'px,' + t.y + 'px) scale(' + t.scaleX + ',' + t.scaleY + ')'

/** Keyframe properties that put a fixed-position ghost exactly on `rect`. */
export function ghostFrame(rect: Rect, radius: number): Record<string, string> {
  return { left: rect.left + 'px', top: rect.top + 'px', width: rect.width + 'px', height: rect.height + 'px', borderRadius: radius + 'px' }
}

/** The grid's identity for transitions: only a change of this (not of the games) animates. */
export const gridKey = (view: string, store: string, onlyInstalled: boolean) => view + '\u0000' + store + '\u0000' + (onlyInstalled ? '1' : '0')
