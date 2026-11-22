// Prepares a release for the repository in RELEASE_REPO (default m72b/GameLobbies) from a
// signed NSIS installer in BUNDLE_DIR (default src-tauri/target/release/bundle/nsis): the setup
// .exe and the .sig written by `tauri signer sign` (or by a build with tauri.release.conf.json).
// It:
//   - copies the signed NSIS installer to <outDir> under a name without spaces (GitHub
//     rewrites spaces in asset names, which would break the manifest URL), and
//   - writes <outDir>/latest.json in Tauri's static updater format. Its installer URL uses the
//     repository path in DOWNLOAD_REPO (default m72b/GameLobby, the name before the rename, which
//     GitHub redirects to RELEASE_REPO): installed 0.5.0-0.9.0 copies accept installers only
//     under /m72b/GameLobby/releases/download/ (see docs/RELEASING.md).
//
// Usage: node .github/scripts/updater-manifest.mjs <version> <outDir> [notesFile]
// Prints the bare asset name (the installer's file name in <outDir>) and, on GitHub Actions,
// also sets the step output `asset` to it. Verify the signature cryptographically with
// verify-minisign.mjs; this script only checks that it names the version.
import { appendFileSync, copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const RELEASE_REPO = process.env.RELEASE_REPO || 'm72b/GameLobbies'
const DOWNLOAD_REPO = process.env.DOWNLOAD_REPO || 'm72b/GameLobby'
const BUNDLE_DIR = process.env.BUNDLE_DIR || join('src-tauri', 'target', 'release', 'bundle', 'nsis')

function fail(message) {
  console.error(`::error::${message}`)
  process.exit(1)
}

const [version, outDir, notesFile] = process.argv.slice(2)
for (const repo of [RELEASE_REPO, DOWNLOAD_REPO]) {
  if (!/^[A-Za-z0-9-]+\/[A-Za-z0-9._-]+$/.test(repo)) fail(`Not an owner/repository name: ${repo}`)
}
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
  fail(`${signaturePath} is missing: sign the installer first (tauri signer sign --app-version ${version})`)
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
  notes: notes || `GameLobbies ${version}`,
  pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, 'Z'),
  platforms: {
    'windows-x86_64': {
      signature,
      url: `https://github.com/${DOWNLOAD_REPO}/releases/download/v${version}/${encodeURIComponent(assetName)}`
    }
  }
}
writeFileSync(join(outDir, 'latest.json'), JSON.stringify(manifest, null, 2) + '\n')
if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `asset=${assetName}
`)
console.log(assetName)
