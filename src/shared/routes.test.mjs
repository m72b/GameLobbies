// Run with `npm run test:web` (Node strips the TypeScript types of routes.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isEmulatorRoute, isSteamGame, routeLabel, routeTitle } from './routes.ts'

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

const rom = (launchTarget, system = 'nds') => ({ sourceId: 'rom', store: 'Emulator', launchTarget, system })

test('emulator routes show the ROM file name, extension kept', () => {
  const a = rom('D:\\Roms\\NDS\\Castlevania (USA).nds')
  assert.equal(routeLabel(a, [a]), 'Emulator · Castlevania (USA).nds')
  const b = rom('/roms/Game.iso', 'ps2'), c = rom('/roms/Game.chd', 'ps2')
  assert.equal(routeLabel(b, [b, c]), 'Emulator · Game.iso')
  assert.equal(routeLabel(c, [b, c]), 'Emulator · Game.chd')
})

test('identical file names get the parent folder', () => {
  const a = rom('D:\\Roms\\USA\\Game.nds'), b = rom('D:\\Roms\\EU\\Game.nds')
  assert.equal(routeLabel(a, [a, b]), 'Emulator · USA/Game.nds')
  assert.equal(routeLabel(b, [a, b]), 'Emulator · EU/Game.nds')
})

test('non-emulator routes and routes without a path keep their store', () => {
  const steam = { sourceId: 'steam', store: 'Steam', launchTarget: 'steam://x' }
  assert.equal(routeLabel(steam, [steam, rom('a.nds')]), 'Steam')
  assert.equal(routeLabel(rom(''), []), 'Emulator')
})

test('title adds the system name', () => {
  const a = rom('/r/a.nds')
  assert.equal(routeTitle(a, [a], 'Nintendo DS'), 'Emulator · a.nds (Nintendo DS)')
  assert.equal(routeTitle(a, [a]), 'Emulator · a.nds')
})
