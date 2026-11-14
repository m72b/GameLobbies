// Run with `npm run test:web` (Node strips the TypeScript types of storage.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { formatBytes, sortStorage } from './storage.ts'

test('formatBytes: units, rounding and unusable input', () => {
  assert.equal(formatBytes(0), '0 B')
  assert.equal(formatBytes(1023), '1023 B')
  assert.equal(formatBytes(1024), '1 KB')
  assert.equal(formatBytes(1536), '1.5 KB')
  assert.equal(formatBytes(10 * 1024), '10 KB')
  assert.equal(formatBytes(1024 * 1024 * 123.4), '123 MB')
  assert.equal(formatBytes(48.26 * 1024 ** 3), '48.3 GB')
  assert.equal(formatBytes(2 * 1024 ** 4), '2 TB')
  assert.equal(formatBytes(1024 ** 2 - 1), '1 MB')
  for (const bad of [undefined, null, -1, Number.NaN, Number.POSITIVE_INFINITY]) assert.equal(formatBytes(bad), 'Unknown')
})

const entry = (title, bytes, lastPlayed) => ({ gameId: title, title, store: 'Steam', bytes, lastPlayed, partial: false })

test('sortStorage: size puts unknown last, name ignores case, last played puts never-played last', () => {
  const list = [entry('b', 5, 10), entry('A', undefined, undefined), entry('c', 50, undefined), entry('d', 5, 99)]
  assert.deepEqual(sortStorage(list, 'size').map(e => e.title), ['c', 'b', 'd', 'A'])
  assert.deepEqual(sortStorage(list, 'name').map(e => e.title), ['A', 'b', 'c', 'd'])
  assert.deepEqual(sortStorage(list, 'lastPlayed').map(e => e.title), ['d', 'b', 'A', 'c'])
  assert.equal(list[0].title, 'b')
})
