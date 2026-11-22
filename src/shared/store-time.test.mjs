// Run with `npm run test:web` (Node strips the TypeScript types of store-time.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { addedLabel, addedTooltip, additionOf, effectiveLastPlayed, effectiveMinutes, effectiveSeconds, formatHours } from './store-time.ts'

const H = 3600
const game = (id, playtimeSeconds, lastPlayed) => ({ id, title: id, playtimeSeconds, playtimeMinutes: Math.floor(playtimeSeconds / 60), lastPlayed })
const time = (seconds, tracked, extra = {}) => ({ source: 'steam', seconds, trackedSeconds: tracked, effectiveSeconds: Math.max(seconds, tracked), addedSeconds: Math.max(0, seconds - tracked), syncedAt: 5, ...extra })

test('formatHours: hours with one decimal, minutes below an hour', () => {
  assert.equal(formatHours(165 * H), '165 h')
  assert.equal(formatHours(12.5 * H), '12.5 h')
  assert.equal(formatHours(45 * 60), '45 min')
  assert.equal(formatHours(30), '<1 min')
  assert.equal(formatHours(0), '0 min')
  assert.equal(formatHours(Number.NaN), '0 min')
})

test('the label and tooltip read as specified', () => {
  assert.equal(addedLabel(125 * H), 'Added from Steam · 125 h')
  assert.equal(addedTooltip({ storeSeconds: 165 * H, trackedSeconds: 40 * H }), 'Steam reports 165 h. GameLobbies recorded 40 h. The difference is added so playtime matches Steam.')
})

test('effective playtime is the larger figure and never below tracked', () => {
  const times = { a: time(165 * H, 40 * H), b: time(10 * H, 50 * H) }
  assert.equal(effectiveSeconds(game('a', 40 * H), times), 165 * H)
  assert.equal(effectiveSeconds(game('b', 50 * H), times), 50 * H)
  assert.equal(effectiveSeconds(game('none', 7 * H), times), 7 * H)
  // A stale map never lowers what the game itself tracked.
  assert.equal(effectiveSeconds(game('a', 200 * H), times), 200 * H)
  assert.equal(effectiveMinutes(game('a', 40 * H), times), 165 * 60)
})

test('effective last played is the newer of tracked and Steam', () => {
  const times = { a: time(H, 0, { effectiveLastPlayed: 9000 }) }
  assert.equal(effectiveLastPlayed({ id: 'a', lastPlayed: 100 }, times), 9000)
  assert.equal(effectiveLastPlayed({ id: 'a', lastPlayed: 20000 }, times), 20000)
  assert.equal(effectiveLastPlayed({ id: 'x' }, times), 0)
})

test('additionOf: hidden when the difference is zero, otherwise one entry dated at the sync', () => {
  assert.equal(additionOf({ id: 'a', title: 'A' }, undefined), null)
  assert.equal(additionOf({ id: 'a', title: 'A' }, time(10 * H, 10 * H)), null)
  assert.equal(additionOf({ id: 'a', title: 'A' }, time(10 * H, 50 * H)), null)
  const entry = additionOf({ id: 'a', title: 'A' }, time(165 * H, 40 * H))
  assert.deepEqual(entry, { gameId: 'a', gameTitle: 'A', source: 'steam', seconds: 125 * H, storeSeconds: 165 * H, trackedSeconds: 40 * H, syncedAt: 5 })
})

test('mergeActivity: Steam entries sit by their sync date, sessions keep their order and count', async () => {
  const { mergeActivity } = await import('./store-time.ts')
  const sessions = [{ startedAt: 30, id: 1 }, { startedAt: 20, id: 2 }, { startedAt: 10, id: 3 }]
  const entry = (gameId, syncedAt) => ({ gameId, gameTitle: gameId, source: 'steam', seconds: H, storeSeconds: H, trackedSeconds: 0, syncedAt })
  const rows = mergeActivity(sessions, [entry('old', 5), entry('mid', 25), entry('new', 99)])
  assert.deepEqual(rows.map(r => r.kind === 'steam' ? r.entry.gameId : r.session.id), ['new', 1, 'mid', 2, 3, 'old'])
  assert.equal(rows.filter(r => r.kind === 'session').length, 3)
  assert.deepEqual(mergeActivity([], [entry('only', 1)]).map(r => r.kind), ['steam'])
  assert.deepEqual(mergeActivity(sessions, []).map(r => r.kind), ['session', 'session', 'session'])
})

test('GOG figures name GOG', () => {
  assert.equal(addedLabel(2 * 3600, 'gog'), 'Added from GOG · 2 h')
  assert.equal(addedTooltip({ storeSeconds: 7200, trackedSeconds: 3600, source: 'gog' }), 'GOG reports 2 h. GameLobbies recorded 1 h. The difference is added so playtime matches GOG.')
  assert.equal(additionOf({ id: 'gog:1', title: 'W' }, { source: 'gog', seconds: 7200, trackedSeconds: 3600, effectiveSeconds: 7200, addedSeconds: 3600, syncedAt: 1 }).source, 'gog')
})
