// node --test ".github/scripts/*.test.mjs"
// Fixtures: payload.txt signed with `tauri signer sign --app-version 1.2.3` by a throwaway key
// whose private half was deleted after signing; payload.pub is its public key.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { readFileSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'
import { bindsVersion, parsePublicKey, verifyMinisign } from './verify-minisign.mjs'

const here = dirname(fileURLToPath(import.meta.url))
const fixture = name => join(here, 'fixtures', name)
const data = readFileSync(fixture('payload.txt'))
const signature = readFileSync(fixture('payload.txt.sig'), 'utf8')
const pubkey = readFileSync(fixture('payload.pub'), 'utf8')
const appConfig = join(here, '..', '..', 'src-tauri', 'tauri.conf.json')
const appPubkey = JSON.parse(readFileSync(appConfig, 'utf8')).plugins.updater.pubkey

// Rebuilds a .sig with one decoded line replaced.
function editSignature(edit) {
  const lines = Buffer.from(signature.trim(), 'base64').toString('utf8').split('\n')
  edit(lines)
  return Buffer.from(lines.join('\n')).toString('base64')
}

test('a genuine signature verifies and carries the signed version', () => {
  const trusted = verifyMinisign(data, signature, pubkey)
  assert.ok(bindsVersion(trusted, '1.2.3'), trusted)
  assert.ok(!bindsVersion(trusted, '1.2.4'))
  assert.ok(!bindsVersion(trusted, '1.2'))
})

test('a changed file is rejected', () => {
  const tampered = Buffer.concat([data, Buffer.from('x')])
  assert.throws(() => verifyMinisign(tampered, signature, pubkey), /does not match the file/)
})

test('a different key is rejected by key id', () => {
  assert.throws(() => verifyMinisign(data, signature, appPubkey), /Key id mismatch/)
})

test('a changed trusted comment is rejected', () => {
  const forged = editSignature(lines => { lines[2] = lines[2].replace('version:1.2.3', 'version:9.9.9') })
  assert.throws(() => verifyMinisign(data, forged, pubkey), /trusted comment signature does not match/)
})

test('a changed signature is rejected', () => {
  const forged = editSignature(lines => {
    const bytes = Buffer.from(lines[1], 'base64')
    bytes[20] ^= 1
    lines[1] = bytes.toString('base64')
  })
  assert.throws(() => verifyMinisign(data, forged, pubkey), /does not match the file/)
})

test('a key id forged to match does not help', () => {
  const app = parsePublicKey(appPubkey)
  const forged = editSignature(lines => {
    const bytes = Buffer.from(lines[1], 'base64')
    app.keyId.copy(bytes, 2)
    lines[1] = bytes.toString('base64')
  })
  assert.throws(() => verifyMinisign(data, forged, appPubkey), /does not match the file/)
})

test('the app pubkey parses', () => {
  const { keyId } = parsePublicKey(appPubkey)
  assert.equal(Buffer.from(keyId).reverse().toString('hex').toUpperCase(), '037C6D4EC9885F04')
})

test('the command line checks the version and exits non-zero on failure', () => {
  const script = join(here, 'verify-minisign.mjs')
  const config = JSON.stringify({ plugins: { updater: { pubkey } } })
  const configPath = join(process.env.RUNNER_TEMP ?? here, `verify-test-${process.pid}.json`)
  writeFileSync(configPath, config)
  try {
    const out = execFileSync(process.execPath, [script, fixture('payload.txt'), fixture('payload.txt.sig'), '--version', '1.2.3', '--config', configPath], { encoding: 'utf8' })
    assert.match(out, /Signature OK/)
    assert.throws(() => execFileSync(process.execPath, [script, fixture('payload.txt'), fixture('payload.txt.sig'), '--version', '1.2.4', '--config', configPath], { stdio: 'pipe' }))
    assert.throws(() => execFileSync(process.execPath, [script, fixture('payload.txt'), fixture('payload.txt.sig')], { stdio: 'pipe' }), 'the app key did not sign the fixture')
  } finally {
    rmSync(configPath, { force: true })
  }
})
