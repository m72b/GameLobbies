// Run with `npm run test:web` (Node strips the TypeScript types of routes.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isEmulatorRoute } from './routes.ts'

test('ROM routes get an emulator picker', () => {
  assert.equal(isEmulatorRoute({ sourceId: 'rom', store: 'Emulator', system: 'ps2' }), true)
  assert.equal(isEmulatorRoute({ sourceId: 'rom', store: 'Emulator' }), true, 'system unknown: still a ROM')
  assert.equal(isEmulatorRoute({ sourceId: 'rom', store: 'Emulator', system: 'switch' }), true)
})

test('PC, local and store routes never do', () => {
  assert.equal(isEmulatorRoute({ sourceId: 'local', store: 'Custom', system: 'pc' }), false)
  assert.equal(isEmulatorRoute({ sourceId: 'local', store: 'Custom' }), false)
  assert.equal(isEmulatorRoute({ sourceId: 'manual', store: 'Custom', system: 'pc' }), false)
  assert.equal(isEmulatorRoute({ sourceId: 'steam', store: 'Steam' }), false)
  assert.equal(isEmulatorRoute({ sourceId: 'epic', store: 'Epic Games' }), false)
})
