// Run with `npm run test:web` (Node strips the TypeScript types of accounts.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { accountLine, canSync, choicesFor, defaultChoice, setupSteps } from './accounts.ts'

const steam = { store: 'steam', name: 'Steam', available: true, mode: 'account', signedIn: true, keySet: true, signingIn: false, last: null, notice: null }

test('account lines follow sign-in, mode and the Steam key', () => {
  assert.equal(accountLine(steam).state, 'ok')
  assert.equal(accountLine({ ...steam, keySet: false }).pill, 'Key missing')
  assert.equal(accountLine({ ...steam, mode: 'installed' }).pill, 'Installed only')
  assert.equal(accountLine({ ...steam, signedIn: false, mode: 'installed' }).state, 'off')
  assert.equal(accountLine({ ...steam, signingIn: true }).pill, 'Signing in')
  assert.equal(accountLine({ ...steam, store: 'gog', name: 'GOG', available: false, signedIn: false }).state, 'soon')
})

test('sync needs a signed-in account, the account mode and (Steam) the key', () => {
  assert.equal(canSync(steam), true)
  assert.equal(canSync({ ...steam, keySet: false }), false)
  assert.equal(canSync({ ...steam, mode: 'installed' }), false)
  assert.equal(canSync({ ...steam, signedIn: false }), false)
  assert.equal(canSync({ ...steam, signingIn: true }), false)
  assert.equal(canSync({ ...steam, available: false }), false)
})

test('the Steam step appears only when Steam sign-in was chosen', () => {
  assert.deepEqual(setupSteps({ steam: 'signin' }), ['welcome', 'stores', 'choices', 'steam', 'folders', 'keys', 'done'])
  assert.deepEqual(setupSteps({ steam: 'installed' }), ['welcome', 'stores', 'choices', 'folders', 'keys', 'done'])
  assert.deepEqual(setupSteps({}), ['welcome', 'stores', 'choices', 'folders', 'keys', 'done'])
})

test('choices cover every store and never ask for a sign-in that is not offered', () => {
  const stores = [
    { id: 'steam', name: 'Steam', signIn: true, note: '' },
    { id: 'gog', name: 'GOG', signIn: false, note: '' },
    { id: 'ea', name: 'EA app', signIn: false, note: '' }
  ]
  assert.equal(defaultChoice(stores[0]), 'signin')
  assert.equal(defaultChoice(stores[1]), 'installed')
  assert.deepEqual(choicesFor(stores, ['steam', 'gog'], { gog: 'signin' }), { steam: 'signin', gog: 'installed', ea: 'skip' })
  assert.deepEqual(choicesFor(stores, [], {}), { steam: 'skip', gog: 'skip', ea: 'skip' })
  assert.deepEqual(choicesFor(stores, ['steam'], { steam: 'installed' }), { steam: 'installed', gog: 'skip', ea: 'skip' })
})

test('a running Steam sign-in points to the browser', () => {
  assert.equal(accountLine({ ...steam, signingIn: true }).label, 'Continue signing in to Steam in your browser')
})
