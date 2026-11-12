// Prepares a release for the repository in RELEASE_REPO (default m72b/GameLobby-releases) after
// `tauri build --config src-tauri/tauri.release.conf.json`:
//   - copies the signed NSIS installer to <outDir> under a name without spaces (GitHub
//     rewrites spaces in asset names, which would break the manifest URL), and
//   - writes <outDir>/latest.json in Tauri's static updater format.
//
// Usage: node .github/scripts/updater-manifest.mjs <version> <outDir> [notesFile]
// Prints the installer's path in <outDir>.
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const RELEASE_REPO = process.env.RELEASE_REPO || 'm72b/GameLobby-releases'
const BUNDLE_DIR = join('src-tauri', 'target', 'release', 'bundle', 'nsis')

function fail(message) {
  console.error(`::error::${message}`)
  process.exit(1)
}

const [version, outDir, notesFile] = process.argv.slice(2)
if (!/^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/.test(RELEASE_REPO)) fail(`Not an owner/repository name: ${RELEASE_REPO}`)
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version ?? '')) fail(`Not a release version: ${version}`)
if (!outDir) fail('Missing the output folder')
if (!existsSync(BUNDLE_DIR)) fail(`${BUNDLE_DIR} does not exist; run the release build first`)

// Tauri 2 with createUpdaterArtifacts: true signs the NSIS setup itself:
// "<productName>_<version>_x64-setup.exe" plus "<same>.sig".
const suffix = `_${version}_x64-setup.exe`
const installers = readdirSync(BUNDLE_DIR).filter(name => name.endsWith(suffix))
if (installers.length !== 1) fail(`Expected one *${suffix} in ${BUNDLE_DIR}, found: ${installers.join(', ') || 'none'}`)
const installer = installers[0]
const signaturePath = join(BUNDLE_DIR, `${installer}.sig`)
if (!existsSync(signaturePath)) {
  fail(`${signaturePath} is missing: build with --config src-tauri/tauri.release.conf.json and the TAURI_SIGNING_PRIVATE_KEY secret`)
}
const signature = readFileSync(signaturePath, 'utf8').trim()

// The app sets requireSignedVersion: the signature's trusted comment must name this version.
const decoded = Buffer.from(signature, 'base64').toString('utf8')
const trusted = decoded.split('\n').find(line => line.startsWith('trusted comment:')) ?? ''
if (!trusted.split('\t').includes(`version:${version}`)) {
  fail(`The signature is not bound to version ${version} (trusted comment: "${trusted}"); the app would reject it`)
}

const assetName = installer.replace(/[^A-Za-z0-9._-]+/g, '')
mkdirSync(outDir, { recursive: true })
copyFileSync(join(BUNDLE_DIR, installer), join(outDir, assetName))

const notes = notesFile && existsSync(notesFile) ? readFileSync(notesFile, 'utf8').trim() : ''
const manifest = {
  version,
  notes: notes || `Game Lobby ${version}`,
  pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
  platforms: {
    'windows-x86_64': {
      signature,
      url: `https://github.com/${RELEASE_REPO}/releases/download/v${version}/${encodeURIComponent(assetName)}`
    }
  }
}
writeFileSync(join(outDir, 'latest.json'), JSON.stringify(manifest, null, 2) + '\n')
console.log(join(outDir, assetName))
