// Run with `npm run test:web` (Node strips the TypeScript types of motion.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { CLOSE, EASE, EASE_MOVE, OPEN, REDUCED_MS, REDUCED_PAGE_MS, cascadeDelays, centerOf, coverTransform, durationFor, gridKey, insetClip, isRectVisible, motionLevel, openTotalMs, parseMotion, relativeTo, transformCss } from './motion.ts'

test('durations: full, short fade with reduced motion, none when animations are off', () => {
  assert.equal(durationFor(300, 'full'), 300)
  assert.equal(durationFor(300, 'reduced'), REDUCED_MS)
  assert.equal(durationFor(40, 'reduced'), 40)
  assert.equal(durationFor(300, 'off'), 0)
  assert.equal(durationFor(-5, 'full'), 0)
})

test('motion level from the OS preference and the Settings switch', () => {
  assert.equal(motionLevel(false, 'system'), 'full')
  assert.equal(motionLevel(true, 'system'), 'reduced')
  assert.equal(motionLevel(false, 'reduced'), 'reduced')
  assert.equal(motionLevel(false, 'off'), 'off')
  // Full always animates, whatever Windows prefers; Off and the Animations switch still win.
  assert.equal(motionLevel(true, 'full'), 'full')
  assert.equal(motionLevel(false, 'full'), 'full')
  assert.equal(motionLevel(true, 'full', false), 'off')
  assert.equal(motionLevel(true, 'off'), 'off')
  // The older Animations switch still turns everything off.
  assert.equal(motionLevel(false, 'system', false), 'off')
  assert.equal(parseMotion('reduced'), 'reduced')
  assert.equal(parseMotion('off'), 'off')
  assert.equal(parseMotion('full'), 'full')
  assert.equal(parseMotion(undefined), 'full')
  assert.equal(parseMotion('weird'), 'full')
})

test('cover transform: one uniform scale that covers the target, centred on it', () => {
  const poster = { left: 100, top: 200, width: 150, height: 225 }
  const hero = { left: 58, top: 38, width: 1000, height: 400 }
  const t = coverTransform(poster, hero)
  assert.ok(Math.abs(t.scale - 1000 / 150) < 1e-9)
  assert.ok(poster.width * t.scale >= hero.width && poster.height * t.scale >= hero.height)
  assert.equal(t.x, centerOf(hero).x - centerOf(poster).x)
  assert.equal(t.y, centerOf(hero).y - centerOf(poster).y)
  assert.deepEqual(coverTransform(hero, hero), { x: 0, y: 0, scale: 1 })
  assert.equal(coverTransform(poster, { left: 0, top: 0, width: 0, height: 0 }).scale, 1)
  assert.equal(transformCss({ x: 1, y: 2, scale: 3 }), 'translate(1px,2px) scale(3)')
  // Closing is the same maths the other way round.
  assert.ok(coverTransform(hero, poster).scale < 1)
})

test('inset clip crops a layer to a rect with rounded corners', () => {
  assert.equal(insetClip({ left: 10, top: 20, width: 100, height: 50 }, 400, 300, 8), 'inset(20px 290px 230px 10px round 8px)')
  assert.equal(insetClip({ left: 0, top: 0, width: 400, height: 300 }, 400, 300, 0), 'inset(0px 0px 0px 0px round 0px)')
  assert.deepEqual(relativeTo({ left: 70, top: 90, width: 5, height: 6 }, { left: 58, top: 38, width: 1, height: 1 }), { left: 12, top: 52, width: 5, height: 6 })
})

test('open sequence timing: clear, ordered, and fully inside the zoom', () => {
  assert.ok(OPEN.zoomMs >= 420 && OPEN.zoomMs <= 480)
  assert.ok(OPEN.liftMs <= 100 && OPEN.liftScale === 1.04)
  assert.ok(OPEN.cascadeStartMs >= 200 && OPEN.cascadeStartMs <= OPEN.zoomDelayMs + OPEN.zoomMs)
  assert.equal(OPEN.cascadeStaggerMs, 60)
  assert.ok(OPEN.dimTo > 0 && OPEN.dimTo <= 0.4 && OPEN.recedeScale === 0.98)
  // The cover has finished fading when the zoom ends.
  assert.ok(OPEN.coverFadeDelayMs + OPEN.coverFadeMs <= OPEN.zoomDelayMs + OPEN.zoomMs + 10)
  assert.equal(OPEN.noCardMs, 300)
  assert.ok(CLOSE.contentMs === 120 && CLOSE.shrinkMs >= 320 && CLOSE.shrinkMs <= 360)
  assert.equal(REDUCED_PAGE_MS, 120)
  assert.deepEqual(cascadeDelays(3, 260, 60), [260, 320, 380])
  assert.deepEqual(cascadeDelays(0, 260, 60), [])
  assert.equal(openTotalMs(3), Math.max(510, 260 + 120 + 300))
})

test('curves never overshoot', () => {
  for (const curve of [EASE, EASE_MOVE]) {
    const [, , y1, , y2] = /cubic-bezier\(([^,]+),([^,]+),([^,]+),([^)]+)\)/.exec(curve).map(Number)
    assert.ok(y1 >= 0 && y1 <= 1 && y2 >= 0 && y2 <= 1, curve)
  }
  assert.equal(EASE_MOVE, 'cubic-bezier(.65,0,.35,1)')
})

test('rect visibility: a card scrolled out of the grid is not a target', () => {
  const grid = { left: 58, top: 150, width: 1000, height: 500 }
  assert.equal(isRectVisible({ left: 100, top: 200, width: 150, height: 225 }, grid), true)
  assert.equal(isRectVisible({ left: 100, top: 600, width: 150, height: 225 }, grid), false)
  assert.equal(isRectVisible({ left: 100, top: 20, width: 150, height: 225 }, grid), false)
  assert.equal(isRectVisible({ left: 100, top: -500, width: 150, height: 225 }, grid), false)
  assert.equal(isRectVisible({ left: 0, top: 0, width: 0, height: 10 }, grid), false)
  assert.equal(isRectVisible({ left: 100, top: 560, width: 150, height: 225 }, grid, 0.1), true)
})

test('grid key changes with the view, the launcher or store, and Installed/All only', () => {
  const a = gridKey('All Games', 'All Games', true)
  assert.equal(a, gridKey('All Games', 'All Games', true))
  assert.notEqual(a, gridKey('Favorites', 'All Games', true))
  assert.notEqual(a, gridKey('All Games', 'launcher:x', true))
  assert.notEqual(a, gridKey('All Games', 'All Games', false))
  assert.notEqual(gridKey('a', 'bc', true), gridKey('ab', 'c', true))
})
