import { EASE, EASE_MOVE, durationFor, motionLevel, parseMotion, type MotionLevel } from '../shared/motion'
import type { MotionPreference } from '../shared/types'

const query = () => { try { return window.matchMedia('(prefers-reduced-motion: reduce)') } catch { return null } }

/** Whether Windows asks for reduced motion (its Animation effects switch is off). */
export const osPrefersReducedMotion = (): boolean => !!query()?.matches

/** The motion the user wants right now: the `data-motion` attribute that {@link applyMotion} keeps on <html>. */
export function currentMotion(): MotionLevel {
  return parseMotion(document.documentElement.dataset.motion)
}

/**
 * Puts the effective motion level on <html data-motion="full|reduced|off"> for CSS and
 * {@link currentMotion}. "Follow Windows" tracks the OS setting while the app runs; "Full" ignores it. Returns a cleanup.
 */
export function applyMotion(preference: MotionPreference, animationsOn: boolean): () => void {
  const media = query()
  const set = () => { document.documentElement.dataset.motion = motionLevel(!!media?.matches, preference, animationsOn) }
  set()
  media?.addEventListener?.('change', set)
  return () => media?.removeEventListener?.('change', set)
}

/** `element.animate` that tolerates a missing Web Animations API; a zero duration returns null (the caller skips it). */
export function play(element: Element, keyframes: Keyframe[], ms: number, options: KeyframeAnimationOptions = {}): Animation | null {
  if (!(ms > 0) || typeof element.animate !== 'function') return null
  return element.animate(keyframes, { duration: ms, easing: EASE, ...options })
}

export { EASE, EASE_MOVE, durationFor }
