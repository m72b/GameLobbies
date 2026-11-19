// Run with `npm run test:web` (Node strips the TypeScript types of api-keys.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { igdbSummary, keyInfo, keyOrder, pillWord, sgdbSummary, steamSummary } from './api-keys.ts'

test('SteamGridDB: set or not set', () => {
  assert.deepEqual(sgdbSummary(true), { state: 'set', label: 'Key saved' })
  assert.deepEqual(sgdbSummary(false), { state: 'unset', label: 'No key set' })
})

test('IGDB: both fields are needed', () => {
  assert.equal(igdbSummary({ clientIdSet: true, clientSecretSet: true }).state, 'set')
  assert.deepEqual(igdbSummary({ clientIdSet: true, clientSecretSet: false }), { state: 'partial', label: 'Client Secret missing' })
  assert.deepEqual(igdbSummary({ clientIdSet: false, clientSecretSet: true }), { state: 'partial', label: 'Client ID missing' })
  assert.equal(igdbSummary({ clientIdSet: false, clientSecretSet: false }).state, 'unset')
})

test('Steam: key and SteamID are both needed', () => {
  const none = { keySet: false, steamIdSet: false, last: null }
  assert.equal(steamSummary({ ...none, keySet: true, steamIdSet: true }).state, 'set')
  assert.equal(steamSummary({ ...none, keySet: true }).label, 'SteamID missing')
  assert.equal(steamSummary({ ...none, steamIdSet: true }).label, 'API key missing')
  assert.equal(steamSummary(none).state, 'unset')
})

test('every key has a name and what it unlocks; the status words are plain', () => {
  assert.equal(keyOrder.length, 3)
  for (const id of keyOrder) assert.ok(keyInfo[id].name && keyInfo[id].unlocks)
  assert.deepEqual(['set', 'partial', 'unset'].map(pillWord), ['Set', 'Incomplete', 'Not set'])
})
