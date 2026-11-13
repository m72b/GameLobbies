// Run with `npm run test:web` (Node strips the TypeScript types of routes.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isEmulatorRoute, isSteamGame } from './routes.ts'

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

test('Steam imports need a Steam route with a numeric app id', () => {
  assert.equal(isSteamGame({ routes: [{ sourceId: 'steam', appId: '620' }] }), true)
  assert.equal(isSteamGame({ routes: [{ sourceId: 'steam' }] }), false)
  assert.equal(isSteamGame({ routes: [{ sourceId: 'steam', appId: '62a' }] }), false)
  assert.equal(isSteamGame({ routes: [{ sourceId: 'epic', appId: '620' }] }), false)
  assert.equal(isSteamGame({ routes: [] }), false)
})
