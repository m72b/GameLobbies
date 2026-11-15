// Run with `npm run test:web` (Node strips the TypeScript types of launchers.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { LAUNCHER_ICONS, assignTargetLabel, assignedLauncherIds, canScanFolders, isAssignedRoute, playLauncher, displayStore, filterLauncherId, formatArgv, gameLauncher, gameMatchesFilter, identifierProblem, isHexColor, isLabelOnly, launcherFilter, launcherIdOf, launcherKinds, launcherOfRoute, launcherRoots, libraryRoots, previewArgv, routeLauncherId, scanSummaryLines, toggleKind, tokenizeTemplate } from './launchers.ts'

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
  for (const ok of ['Hollow Knight', '1a2b-3c4d', 'legendary:Fortnite@main#1', "Tom and Jerry (1999), 'Gold'+!", '  padded  ', 'user@host', 'Game: Subtitle', '1:2', 'a'.repeat(200)]) {
    assert.equal(identifierProblem(ok), '', ok)
  }
  for (const bad of ['', '   ', '-x', 'Tom & Jerry', '&', '@args.txt', ' @x', 'C:evil.exe', 'c:', 'Z:x y', ' --exec', '/c', 'a/b', 'a\\b', '..', 'a..b', 'a\nb', 'a\tb', 'a"b', 'a;b', 'a|b', 'a%b', 'a=b', 'a\uFF3Cb', 'a\u2215b', 'C\uFF1Ax', 'Pokémon', 'a'.repeat(201)]) {
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

test('labelled routes belong to their launcher, but only launcher routes run through it', () => {
  const labelled = route('local', { launcherId: 'abc' })
  assert.equal(routeLauncherId(labelled), 'abc')
  assert.equal(launcherIdOf(labelled), undefined, 'a label never makes a launcher route')
  assert.equal(routeLauncherId(route('launcher:abc', { launcherId: 'other' })), 'abc', 'the source wins')
  assert.equal(routeLauncherId(route('rom', { launcherId: '' })), undefined)
  assert.equal(launcherOfRoute(labelled, [heroic]), heroic)
  assert.equal(launcherOfRoute(labelled, []), undefined)
  const game = { store: 'Emulator', routes: [route('rom', { launcherId: 'abc' })] }
  assert.equal(gameMatchesFilter(game, launcherFilter('abc')), true)
  assert.equal(gameMatchesFilter(game, launcherFilter('other')), false)
})

test('target kinds: older launchers, canonical order and at least one', () => {
  assert.deepEqual(launcherKinds({ targetKind: 'folder' }), ['folder'])
  assert.deepEqual(launcherKinds({ targetKind: 'file', targetKinds: ['identifier', 'file'] }), ['file', 'identifier'])
  assert.deepEqual(toggleKind(['identifier'], 'file'), ['file', 'identifier'])
  assert.deepEqual(toggleKind(['file', 'identifier'], 'file'), ['identifier'])
  assert.deepEqual(toggleKind(['file'], 'file'), ['file'], 'the last kind stays')
  assert.deepEqual(toggleKind(['file', 'folder'], 'folder', ['folder']), ['file', 'folder'], 'a locked kind stays')
  const program = { executable: 'D:\\P\\p.exe', targetKind: 'identifier', targetKinds: ['identifier'] }
  assert.equal(isLabelOnly(program), false)
  assert.equal(canScanFolders(program), false, 'identifiers only: nothing to scan for')
  assert.equal(canScanFolders({ ...program, targetKinds: ['identifier', 'folder'] }), true)
  assert.equal(canScanFolders({ executable: '', targetKind: 'identifier', targetKinds: ['identifier'] }), true, 'no program')
})

test('launcher games folders are listed with their launcher, not as library folders', () => {
  const roots = [{ id: 'r1', kind: 'games', launcherId: 'abc' }, { id: 'r2', kind: 'games' }, { id: 'r3', kind: 'emulators' }]
  assert.deepEqual(launcherRoots(roots, 'abc').map(r => r.id), ['r1'])
  assert.deepEqual(launcherRoots(roots, 'x'), [])
  assert.deepEqual(libraryRoots(roots).map(r => r.id), ['r2', 'r3'])
})

test('the scan summary lists every count', () => {
  const summary = {
    roots: [{ id: 'r', ok: true }, { id: 'bad', ok: false }],
    emulators: { added: 3, updated: 1, unavailable: 0 },
    games: { added: 9, updated: 0, removedOrUninstalled: 0, skipped: 2, failed: 1, removalsHeld: 0, routesAdded: 2, romsMatched: 3, needsReview: 4 },
    review: 4, unsupportedSamples: [], durationMs: 10
  }
  assert.deepEqual(Object.fromEntries(scanSummaryLines(summary)), {
    'Games added': '9',
    'Routes added to existing games': '2',
    'Emulators discovered': '3 (1 updated)',
    'ROMs matched to an emulator': '3',
    'Needs review': '4 items',
    Skipped: '2 items',
    Errors: '2 errors'
  })
  const empty = { ...summary, roots: [], emulators: { added: 0, updated: 0, unavailable: 0 }, games: { added: 0, updated: 0, removedOrUninstalled: 0, skipped: 1, failed: 0, removalsHeld: 0 }, review: 0 }
  const lines = Object.fromEntries(scanSummaryLines(empty))
  assert.equal(lines['Routes added to existing games'], '0', 'older summaries lack the new counts')
  assert.equal(lines['Needs review'], 'Nothing')
  assert.equal(lines.Skipped, '1 item')
  assert.equal(lines.Errors, 'None')
})

test('built-in icons are a fixed list of 24 unique names', () => {
  assert.equal(LAUNCHER_ICONS.length, 24)
  assert.equal(new Set(LAUNCHER_ICONS).size, 24)
  for (const name of LAUNCHER_ICONS) assert.match(name, /^[a-z0-9-]+$/)
})

test('assignments from the game page are told apart from a launcher\'s own games', () => {
  const app = { ...heroic, id: 'app', name: 'App', executable: '' }
  const withProgram = { ...heroic, executable: 'D:\\Heroic\\Heroic.exe' }
  const game = {
    store: 'Steam',
    routes: [
      route('launcher:abc', { assigned: true, launchTarget: '400' }),
      route('steam', { store: 'Steam', appId: '400', launcherId: 'app', assigned: true }),
      route('launcher:own')
    ]
  }
  assert.equal(isAssignedRoute(game.routes[0]), true)
  assert.equal(isAssignedRoute(game.routes[2]), false)
  assert.deepEqual(assignedLauncherIds(game), ['abc', 'app'])
  assert.deepEqual(assignedLauncherIds({ routes: [route('launcher:abc'), route('local', { launcherId: 'app' })] }), [], 'Add game and games folders are not assignments')
  // The game shows under the launchers it was assigned to.
  assert.equal(gameMatchesFilter(game, launcherFilter('abc')), true)
  assert.equal(gameMatchesFilter(game, launcherFilter('app')), true)
  // Play names the launcher it runs through; a label-only launcher runs nothing.
  assert.equal(playLauncher(game.routes[0], [withProgram, app])?.name, 'Heroic')
  assert.equal(playLauncher(game.routes[1], [withProgram, app]), undefined)
  assert.equal(playLauncher(route('launcher:app'), [app]), undefined)
  assert.equal(playLauncher(undefined, [withProgram]), undefined)
})

test('assignment targets read as their kind and value', () => {
  const routes = [{ store: 'Steam' }, { store: 'Custom' }]
  assert.equal(assignTargetLabel({ routeIndex: 0, kind: 'identifier', target: '400' }, routes), 'Store id · 400 (Steam)')
  assert.equal(assignTargetLabel({ routeIndex: 1, kind: 'file', target: 'D:\\Games\\HK\\hk.exe' }, routes), 'File · hk.exe')
  assert.equal(assignTargetLabel({ routeIndex: 1, kind: 'folder', target: 'D:\\Games\\HK' }, routes), 'Folder · HK')
  assert.equal(assignTargetLabel({ routeIndex: 9, kind: 'identifier', target: 'x' }, routes), 'Store id · x')
})
