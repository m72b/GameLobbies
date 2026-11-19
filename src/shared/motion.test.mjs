// Run with `npm run test:web` (Node strips the TypeScript types of motion.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { REDUCED_MS, durationFor, flipCss, flipTransform, ghostFrame, gridKey, isRectVisible, motionLevel, parseMotion } from './motion.ts'

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
  assert.equal(motionLevel(true, 'off'), 'off')
  // The older Animations switch still turns everything off.
  assert.equal(motionLevel(false, 'system', false), 'off')
  assert.equal(parseMotion('reduced'), 'reduced')
  assert.equal(parseMotion('off'), 'off')
  assert.equal(parseMotion('full'), 'full')
  assert.equal(parseMotion(undefined), 'full')
  assert.equal(parseMotion('weird'), 'full')
})

test('flip: the card rect maps onto the full rect', () => {
  const card = { left: 100, top: 200, width: 150, height: 225 }
  const full = { left: 58, top: 38, width: 1000, height: 400 }
  const t = flipTransform(card, full)
  assert.equal(t.x, 42)
  assert.equal(t.y, 162)
  assert.equal(t.scaleX, 0.15)
  assert.equal(t.scaleY, 0.5625)
  assert.equal(flipCss(t), 'translate(42px,162px) scale(0.15,0.5625)')
  assert.deepEqual(flipTransform(full, full), { x: 0, y: 0, scaleX: 1, scaleY: 1 })
  assert.equal(flipTransform(card, { left: 0, top: 0, width: 0, height: 0 }).scaleX, 1)
})

test('ghost frame puts a fixed element on a rect', () => {
  assert.deepEqual(ghostFrame({ left: 1, top: 2, width: 3, height: 4 }, 10), { left: '1px', top: '2px', width: '3px', height: '4px', borderRadius: '10px' })
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
