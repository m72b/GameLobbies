// Run with `npm run test:web`. Game Lobby's own page freezes Object.prototype from its first
// script (Tauri's app-wide freezePrototype is off so store sign-in pages aren't affected).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const read = path => readFileSync(new URL(path, import.meta.url), 'utf8')

test('index.html loads the freeze script before the app module', () => {
  const html = read('../renderer/index.html')
  const freeze = html.indexOf('<script src="/freeze-prototype.js"></script>')
  const app = html.indexOf('<script type="module"')
  assert.ok(freeze > 0 && app > 0 && freeze < app)
  assert.ok(freeze < html.indexOf('</head>'))
  assert.match(read('../renderer/public/freeze-prototype.js'), /^Object\.freeze\(Object\.prototype\)\r?$/m)
  // The app warns (and keeps running) when the freeze did not happen.
  assert.match(read('../renderer/main.tsx'), /Object\.isFrozen\(Object\.prototype\)/)
  const config = JSON.parse(read('../../src-tauri/tauri.conf.json'))
  assert.equal(config.app.security.freezePrototype, false)
})
