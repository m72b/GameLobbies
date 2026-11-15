// Run with `npm run test:web` (Node strips the TypeScript types of launchers.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { LAUNCHER_ICONS, displayStore, filterLauncherId, formatArgv, gameLauncher, gameMatchesFilter, identifierProblem, isHexColor, launcherFilter, launcherIdOf, previewArgv, tokenizeTemplate } from './launchers.ts'

const heroic = { id: 'abc', name: 'Heroic', iconKind: 'builtin', iconValue: 'rocket', color: '#4f8cff' }
const route = (sourceId, extra = {}) => ({ sourceId, store: 'Custom', launchTarget: 'x', ...extra })

test('launcher routes carry the launcher id', () => {
  assert.equal(launcherIdOf(route('launcher:abc')), 'abc')
  assert.equal(launcherIdOf(route('launcher:')), undefined)
  assert.equal(launcherIdOf(route('local')), undefined)
  assert.equal(launcherIdOf(route('steam')), undefined)
})

test('library filters match stores and launchers', () => {
  const game = { store: 'Custom', routes: [route('launcher:abc')] }
  const steam = { store: 'Steam', routes: [route('steam', { store: 'Steam' })] }
  assert.equal(filterLauncherId(launcherFilter('abc')), 'abc')
  assert.equal(filterLauncherId('Steam'), undefined)
  assert.equal(gameMatchesFilter(game, 'All Games'), true)
  assert.equal(gameMatchesFilter(game, launcherFilter('abc')), true)
  assert.equal(gameMatchesFilter(game, launcherFilter('other')), false)
  assert.equal(gameMatchesFilter(game, 'Custom'), true)
  assert.equal(gameMatchesFilter(steam, launcherFilter('abc')), false)
  assert.equal(gameMatchesFilter(steam, 'Steam'), true)
})

test('custom games show their launcher name while it exists', () => {
  const game = { store: 'Custom', routes: [route('local'), route('launcher:abc')] }
  assert.equal(displayStore(game, [heroic]), 'Heroic')
  assert.equal(gameLauncher(game, [heroic]), heroic)
  assert.equal(displayStore(game, []), 'Custom', 'deleted launcher')
  assert.equal(displayStore({ store: 'Steam', routes: [route('launcher:abc')] }, [heroic]), 'Steam')
})

test('identifiers mirror the backend rules', () => {
  for (const ok of ['Hollow Knight', '1a2b-3c4d', 'legendary:Fortnite@main#1', "Tom & Jerry (1999), 'Gold'+!", '  padded  ', 'a'.repeat(200)]) {
    assert.equal(identifierProblem(ok), '', ok)
  }
  for (const bad of ['', '   ', '-x', ' --exec', '/c', 'a/b', 'a\\b', '..', 'a..b', 'a\nb', 'a\tb', 'a"b', 'a;b', 'a|b', 'a%b', 'a=b', 'a\uFF3Cb', 'a\u2215b', 'C\uFF1Ax', 'Pokémon', 'a'.repeat(201)]) {
    assert.notEqual(identifierProblem(bad), '', JSON.stringify(bad))
  }
})

test('colours are #rrggbb', () => {
  assert.equal(isHexColor('#4f8cff'), true)
  assert.equal(isHexColor('#4F8CFF'), true)
  for (const bad of ['#fff', '4f8cff', '#4f8cff00', '#4f8cfg', 'red', '']) assert.equal(isHexColor(bad), false, bad)
})

test('the preview keeps the target as one argument', () => {
  const target = 'D:\\My Games\\a & b "c"'
  assert.deepEqual(previewArgv('--start {target} -x', target), ['--start', target, '-x'])
  assert.deepEqual(previewArgv('--fullscreen', target), ['--fullscreen', target])
  assert.deepEqual(previewArgv('', target), [target])
  assert.deepEqual(previewArgv('"{target}"', target), [target])
  assert.deepEqual(previewArgv('-p "My Profile" {target}', 'x'), ['-p', 'My Profile', 'x'])
  assert.equal(previewArgv('--id={target}', 'x'), null)
  assert.equal(previewArgv('{rom}', 'x'), null)
  assert.equal(previewArgv('"{target}', 'x'), null)
  assert.deepEqual(tokenizeTemplate(`a 'b c' "" d`), ['a', 'b c', '', 'd'])
  assert.equal(formatArgv(['--start', 'My Game', '']), '--start "My Game" ""')
})

test('built-in icons are a fixed list of 24 unique names', () => {
  assert.equal(LAUNCHER_ICONS.length, 24)
  assert.equal(new Set(LAUNCHER_ICONS).size, 24)
  for (const name of LAUNCHER_ICONS) assert.match(name, /^[a-z0-9-]+$/)
})
