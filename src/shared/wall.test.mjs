// Run with `npm run test:web` (Node strips the TypeScript types of wall.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { firstHole, hashKey, layoutWall, placeTiles, visualOrder, wallColumns, wallSpans } from './wall.ts'

const keys = count => Array.from({ length: count }, (_, i) => ({ key: 'shot:' + i + ':https://images.igdb.com/igdb/image/upload/t_1080p/sc' + (i * 7919).toString(36) + '.jpg', aspect: 16 / 9 }))
const shape = span => span.cols + 'x' + span.rows

test('columns: 3 when narrow, 4 from medium widths up', () => {
  assert.equal(wallColumns(0), 3)
  assert.equal(wallColumns(Number.NaN), 3)
  assert.equal(wallColumns(400), 3)
  assert.equal(wallColumns(560), 4)
  assert.equal(wallColumns(3000), 4)
})

test('sizes are deterministic', () => {
  const items = keys(60)
  assert.deepEqual(wallSpans(items, 4), wallSpans(items.map(item => ({ ...item })), 4))
  assert.equal(hashKey('abc'), hashKey('abc'))
  assert.notEqual(hashKey('abc'), hashKey('abd'))
  // Adding pictures later doesn't change the earlier ones.
  assert.deepEqual(wallSpans(items, 4).slice(0, 30), wallSpans(items.slice(0, 30), 4))
})

test('the first tile is the 2x2 feature and the mix is roughly as intended', () => {
  const spans = wallSpans(keys(201), 4)
  assert.equal(shape(spans[0]), '2x2')
  const counts = { '2x2': 0, '2x1': 0, '1x2': 0, '1x1': 0 }
  for (const span of spans.slice(1)) counts[shape(span)]++
  const share = name => counts[name] / 200
  assert.ok(share('2x2') >= 0.07 && share('2x2') <= 0.2, 'big ' + share('2x2'))
  assert.ok(share('2x1') >= 0.17 && share('2x1') <= 0.38, 'wide ' + share('2x1'))
  // 16:9 pictures get the tall shape too, at a lower rate (about 8%).
  assert.ok(share('1x2') >= 0.03 && share('1x2') <= 0.14, 'tall ' + share('1x2'))
  assert.ok(share('1x1') >= 0.38 && share('1x1') <= 0.64, 'small ' + share('1x1'))
})

test('wide pictures are never tall, portrait pictures prefer tall', () => {
  for (const aspect of [2.4, 1.95, 16 / 9 + 0.05]) {
    const wide = wallSpans(keys(200).map(item => ({ ...item, aspect })), 4)
    assert.ok(wide.every(span => shape(span) !== '1x2'), 'aspect ' + aspect)
  }
  const landscape = wallSpans(keys(200).map(item => ({ ...item, aspect: 16 / 9 })), 4)
  assert.ok(landscape.some(span => shape(span) === '1x2'), '16:9 can be tall')
  const portrait = wallSpans(keys(200).map(item => ({ ...item, aspect: 0.6 })), 4).slice(1)
  assert.ok(portrait.every(span => shape(span) === '1x2' || shape(span) === '2x2'))
})

test('no two 2x2 tiles follow each other', () => {
  for (const aspect of [16 / 9, undefined, 0.6, 3]) {
    const spans = wallSpans(keys(300).map(item => ({ ...item, aspect })), 4)
    for (let i = 1; i < spans.length; i++) assert.ok(!(shape(spans[i]) === '2x2' && shape(spans[i - 1]) === '2x2'), 'at ' + i)
  }
})

test('placement packs densely without overlaps and gives the visual order', () => {
  for (const columns of [3, 4]) {
    const spans = wallSpans(keys(40), columns)
    const placed = placeTiles(spans, columns)
    const cells = new Set()
    for (const place of placed) {
      assert.ok(place.col >= 1 && place.col + place.cols - 1 <= columns)
      for (let r = place.row; r < place.row + place.rows; r++) {
        for (let c = place.col; c < place.col + place.cols; c++) {
          const cell = r + ':' + c
          assert.ok(!cells.has(cell), 'overlap at ' + cell)
          cells.add(cell)
        }
      }
    }
    assert.deepEqual(placed[0], { cols: 2, rows: 2, row: 1, col: 1 })
    const order = visualOrder(placed)
    assert.deepEqual([...order].sort((a, b) => a - b), placed.map((_, i) => i))
    for (let i = 1; i < order.length; i++) {
      const a = placed[order[i - 1]]
      const b = placed[order[i]]
      assert.ok(a.row < b.row || (a.row === b.row && a.col < b.col))
    }
  }
  // Spans wider than the grid are clipped to fit.
  assert.deepEqual(placeTiles([{ cols: 2, rows: 1 }], 1), [{ cols: 1, rows: 1, row: 1, col: 1 }])
})

const holesAbove = (placed, columns) => {
  const lastRow = placed.reduce((max, p) => Math.max(max, p.row + p.rows - 1), 0)
  const cells = new Set()
  for (const p of placed) for (let r = p.row; r < p.row + p.rows; r++) for (let c = p.col; c < p.col + p.cols; c++) {
    const cell = r + ':' + c
    assert.ok(!cells.has(cell), 'overlap at ' + cell)
    assert.ok(c >= 1 && c <= columns, 'outside the grid')
    cells.add(cell)
  }
  let above = 0
  let last = 0
  for (let r = 1; r <= lastRow; r++) for (let c = 1; c <= columns; c++) {
    if (cells.has(r + ':' + c)) continue
    if (r < lastRow) above++
    else last++
  }
  return { above, last }
}

test('the wall ends as a clean block: no gaps above the last row', () => {
  for (const columns of [3, 4]) {
    for (let count = 1; count <= 30; count++) {
      for (const aspect of [16 / 9, undefined, 0.7, 2.5]) {
        const items = keys(count).map(item => ({ ...item, aspect }))
        const placed = layoutWall(items, columns)
        assert.equal(placed.length, count)
        const { above, last } = holesAbove(placed, columns)
        assert.equal(above, 0, count + ' tiles, ' + columns + ' columns, aspect ' + aspect)
        assert.ok(last <= columns - 1)
        assert.equal(firstHole(placed, columns), null)
      }
    }
  }
})

test('the layout is deterministic and keeps the feature', () => {
  const items = keys(13)
  assert.deepEqual(layoutWall(items, 3), layoutWall(items.map(item => ({ ...item })), 3))
  const placed = layoutWall(items, 4)
  assert.equal(placed[0].row, 1)
  assert.equal(placed[0].col, 1)
  assert.ok(placed[0].cols >= 2 && placed[0].rows >= 2)
})

test('the reported case: 13 pictures in 3 columns leave no gap on the right', () => {
  const placed = layoutWall(keys(13), 3)
  assert.equal(holesAbove(placed, 3).above, 0)
})
