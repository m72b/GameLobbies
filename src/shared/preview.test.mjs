import { test } from 'node:test'
import assert from 'node:assert/strict'
import { artPreview, previewKind } from './preview.ts'

const base = 'https://cdn2.steamgriddb.com/'
test('previewKind never offers a video as an image', () => {
  assert.equal(previewKind(base + 'a.png', false), 'image')
  assert.equal(previewKind(base + 'a.WEBP', false), 'image')
  assert.equal(previewKind(base + 'a.webm', false), null)
  assert.equal(previewKind(base + 'a.webm', true), 'video')
  assert.equal(previewKind(base + 'a.mp4', true), null)
  assert.equal(previewKind(base + 'a.webm?x=1', true), 'video')
  assert.equal(previewKind('not a url', true), null)
})
test('artPreview falls back to the full image when the thumb is an unplayable video', () => {
  const cover = { thumb: base + 't.webm', url: base + 'h.webp' }
  assert.deepEqual(artPreview(cover, false), { kind: 'image', src: cover.url })
  assert.deepEqual(artPreview(cover, true), { kind: 'video', src: cover.thumb })
  assert.equal(artPreview({ thumb: base + 't.webm', url: base + 'h.webm' }, false), null)
})
