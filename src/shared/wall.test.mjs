// Run with `npm run test:web` (Node strips the TypeScript types of wall.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { wallColumns, wallSpans } from './wall.ts'

test('columns follow the width', () => {
  assert.equal(wallColumns(0), 2)
  assert.equal(wallColumns(Number.NaN), 2)
  assert.equal(wallColumns(300), 2)
  assert.equal(wallColumns(720), 4)
  assert.equal(wallColumns(5000), 8)
})

test('the first picture is the 2x2 feature, the rest follow their shape', () => {
  assert.deepEqual(wallSpans([16 / 9, 16 / 9, 3.5, 0.6, undefined, -1], 4), [
    { cols: 2, rows: 2 },
    { cols: 1, rows: 1 },
    { cols: 2, rows: 1 },
    { cols: 1, rows: 2 },
    { cols: 1, rows: 1 },
    { cols: 1, rows: 1 }
  ])
  // Two columns: the feature takes the full width; panoramas stay one cell.
  assert.deepEqual(wallSpans([1, 3.5], 2), [{ cols: 2, rows: 2 }, { cols: 1, rows: 1 }])
  assert.deepEqual(wallSpans([], 4), [])
})
