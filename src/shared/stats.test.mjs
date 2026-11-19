// Run with `npm run test:web` (Node strips the TypeScript types of stats.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { barWidth, defaultDirection, formatPercent, playtimeBreakdown, sortTop, storeHint } from './stats.ts'

test('formatPercent: whole numbers, one decimal, tiny shares and bad input', () => {
  assert.equal(formatPercent(50), '50%')
  assert.equal(formatPercent(66.7), '66.7%')
  assert.equal(formatPercent(100), '100%')
  assert.equal(formatPercent(0.04), '<0.1%')
  assert.equal(formatPercent(0), '0%')
  assert.equal(formatPercent(100.04), '100%')
  for (const bad of [undefined, null, -5, Number.NaN, Number.POSITIVE_INFINITY]) assert.equal(formatPercent(bad), '0%')
})

test('barWidth: clamped, with a sliver for a tiny share and nothing for none', () => {
  assert.equal(barWidth(1, 4), 25)
  assert.equal(barWidth(5, 4), 100)
  assert.equal(barWidth(0, 4), 0)
  assert.equal(barWidth(1, 0), 0)
  assert.equal(barWidth(1, 100000), 1)
  assert.equal(barWidth(Number.NaN, 4), 0)
})

const row = (title, seconds) => ({ gameId: title, title, seconds, trackedSeconds: seconds, percent: 0, store: 'Steam' })

test('sortTop: time biggest first with title ties, title ignoring case, input untouched', () => {
  const list = [row('b', 10), row('A', 10), row('c', 50)]
  assert.deepEqual(sortTop(list, 'seconds', 'desc').map(r => r.title), ['c', 'A', 'b'])
  assert.deepEqual(sortTop(list, 'seconds', 'asc').map(r => r.title), ['A', 'b', 'c'])
  assert.deepEqual(sortTop(list, 'title', 'asc').map(r => r.title), ['A', 'b', 'c'])
  assert.deepEqual(sortTop(list, 'title', 'desc').map(r => r.title), ['c', 'b', 'A'])
  assert.equal(list[0].title, 'b')
  assert.equal(defaultDirection('title'), 'asc')
  assert.equal(defaultDirection('seconds'), 'desc')
})

test('storeHint: none, one store, several', () => {
  assert.equal(storeHint([]), '')
  assert.equal(storeHint(['Steam']), 'includes Steam playtime')
  assert.equal(storeHint(['Steam', 'GOG']), 'includes Steam and GOG playtime')
  assert.equal(storeHint(['Steam', 'GOG', 'Epic']), 'includes Steam, GOG and Epic playtime')
})

test('playtimeBreakdown: tracked only, or tracked against the store with the larger counted', () => {
  const fmt = s => s + 's'
  assert.equal(playtimeBreakdown({ seconds: 60, trackedSeconds: 60 }, fmt), 'Game Lobby tracked: 60s')
  assert.equal(
    playtimeBreakdown({ seconds: 900, trackedSeconds: 60, storeSeconds: 900, storeSource: 'Steam' }, fmt),
    'Game Lobby tracked: 60s\nSteam reports: 900s\nCounted: 900s (the larger of the two, never added)'
  )
})
