// node --test ".github/scripts/*.test.mjs"
// Runs updater-manifest.mjs on a fake bundle folder (an installer that is not a real one and a
// .sig whose trusted comment names the version; the signature itself is not checked here).
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import test from 'node:test'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const script = join(here, 'updater-manifest.mjs')
const appConfig = JSON.parse(readFileSync(join(here, '..', '..', 'src-tauri', 'tauri.conf.json'), 'utf8'))
const workflow = readFileSync(join(here, '..', 'workflows', 'release.yml'), 'utf8')
const workflowEnv = name => workflow.split(/\r?\n/).map(l => l.trim()).find(l => l.startsWith(`${name}:`))?.slice(name.length + 1).trim()

// The check every installed copy runs on the manifest's installer URL (src-tauri/src/updater.rs,
// download_url_allowed), with the prefix taken from one of its endpoints.
const prefixOf = endpoint => {
  const [owner, repo] = new URL(endpoint).pathname.split('/').slice(1, 3)
  return `/${owner}/${repo}/releases/download/`
}
const allowed = (url, prefix) => {
  const u = new URL(url)
  return u.protocol === 'https:' && u.host === 'github.com' && !u.username && !u.password &&
    u.pathname.length > prefix.length && u.pathname.startsWith(prefix) &&
    !u.pathname.toLowerCase().includes('%2e') && !u.pathname.includes('/../')
}
// Installed 0.5.0-0.9.0 copies have exactly this one endpoint.
const OLD_ENDPOINT = 'https://github.com/m72b/GameLobby/releases/latest/download/latest.json'

function run(version, env = {}) {
  const dir = mkdtempSync(join(tmpdir(), 'gamelobbies-manifest-'))
  try {
    const bundle = join(dir, 'nsis')
    const out = join(dir, 'release')
    mkdirSync(bundle)
    const installer = `GameLobbies_${version}_x64-setup.exe`
    writeFileSync(join(bundle, installer), 'not a real installer')
    const trusted = `trusted comment: timestamp:1\tfile:${installer}\tversion:${version}`
    const sig = Buffer.from(`untrusted comment: test\nAAAA\n${trusted}\nBBBB\n`).toString('base64')
    writeFileSync(join(bundle, `${installer}.sig`), sig)
    const asset = execFileSync(process.execPath, [script, version, out], {
      encoding: 'utf8',
      env: { ...process.env, BUNDLE_DIR: bundle, GITHUB_OUTPUT: '', RELEASE_REPO: '', DOWNLOAD_REPO: '', ...env }
    }).trim()
    return { asset, manifest: JSON.parse(readFileSync(join(out, 'latest.json'), 'utf8')) }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
}

test('the installer URL uses the old repository path, accepted by old and new copies', () => {
  const { asset, manifest } = run('1.2.3', { DOWNLOAD_REPO: workflowEnv('DOWNLOAD_REPO'), RELEASE_REPO: workflowEnv('RELEASE_REPO') })
  // The file name follows productName; old copies don't look at it, only the path and signature.
  assert.equal(asset, 'GameLobbies_1.2.3_x64-setup.exe')
  const url = manifest.platforms['windows-x86_64'].url
  assert.equal(url, 'https://github.com/m72b/GameLobby/releases/download/v1.2.3/GameLobbies_1.2.3_x64-setup.exe')
  assert.ok(allowed(url, prefixOf(OLD_ENDPOINT)), 'refused by installed 0.5.0-0.9.0 copies')
  assert.ok(appConfig.plugins.updater.endpoints.some(e => allowed(url, prefixOf(e))), 'refused by this build')
  assert.equal(manifest.version, '1.2.3')
  assert.equal(manifest.notes, 'GameLobbies 1.2.3')
})

test('the defaults match the release workflow', () => {
  assert.equal(workflowEnv('RELEASE_REPO'), 'm72b/GameLobbies')
  assert.equal(workflowEnv('DOWNLOAD_REPO'), 'm72b/GameLobby')
  const { manifest } = run('2.0.0')
  assert.ok(manifest.platforms['windows-x86_64'].url.startsWith('https://github.com/m72b/GameLobby/releases/download/v2.0.0/'))
})

test('a URL under the new name would lock out installed copies', () => {
  const { manifest } = run('1.2.3', { DOWNLOAD_REPO: 'm72b/GameLobbies' })
  const url = manifest.platforms['windows-x86_64'].url
  assert.ok(!allowed(url, prefixOf(OLD_ENDPOINT)))
})

test('a bad repository name is refused', () => {
  assert.throws(() => run('1.2.3', { DOWNLOAD_REPO: 'm72b/../x y' }), { status: 1 })
})
