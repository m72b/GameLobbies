// Builds native/ (Rust, Node-API) and copies the result to native/lobby_native.node.
// Without a Rust toolchain the build is skipped and Game Lobby keeps using its
// PowerShell fallbacks. Set LOBBY_REQUIRE_NATIVE=1 (as CI does) to fail instead.
import { spawnSync } from 'node:child_process'
import { copyFileSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const crate = join(root, 'native')
const required = process.env.LOBBY_REQUIRE_NATIVE === '1'

function skip(reason) {
  if (required) {
    console.error(`[native] ${reason}`)
    process.exit(1)
  }
  console.warn(`[native] ${reason} Skipping the native addon; the PowerShell fallback stays in use.`)
  process.exit(0)
}

if (process.platform !== 'win32') skip('The native addon currently targets Windows only.')
const probe = spawnSync('cargo', ['--version'], { stdio: 'ignore' })
if (probe.error || probe.status !== 0) skip('Rust (cargo) was not found on PATH.')

const build = spawnSync('cargo', ['build', '--release', '--manifest-path', join(crate, 'Cargo.toml')], { stdio: 'inherit' })
if (build.status !== 0) process.exit(build.status ?? 1)

const output = join(crate, 'target', 'release', 'lobby_native.dll')
if (!existsSync(output)) {
  console.error(`[native] Expected build output was not found: ${output}`)
  process.exit(1)
}
copyFileSync(output, join(crate, 'lobby_native.node'))
console.log('[native] Built native/lobby_native.node')
