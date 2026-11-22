// Verifies a Tauri updater signature (minisign) before a release is published, independently
// of the Tauri CLI that made it. Uses only Node's built-in crypto (Ed25519, BLAKE2b-512).
//
// Usage: node .github/scripts/verify-minisign.mjs <file> <signature> [--version X.Y.Z] [--config <tauri.conf.json>]
//   <signature>  path to the .sig file, or "-" plus SIGNATURE in the environment (base64, as in latest.json)
//   --version    also require "version:X.Y.Z" in the signed trusted comment (requireSignedVersion)
//   --config     where the public key is read from (plugins.updater.pubkey); default src-tauri/tauri.conf.json
//
// Minisign formats (all base64 inside Tauri's extra base64 layer):
//   public key: "untrusted comment: …\n" + base64( "Ed" | key id (8) | Ed25519 key (32) )
//   signature:  "untrusted comment: …\n" + base64( alg (2) | key id (8) | signature (64) ) + "\n"
//               "trusted comment: <text>\n" + base64( global signature (64) )
//   alg "ED": the signature covers BLAKE2b-512(file) (prehashed, what Tauri produces);
//   alg "Ed": legacy, the signature covers the file itself.
//   The global signature covers signature (64) || <text>.
import { createHash, createPublicKey, verify } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { pathToFileURL } from 'node:url'

function fromTauriBase64(value, what) {
  const text = Buffer.from(String(value).trim(), 'base64').toString('utf8')
  const lines = text.split(/\r?\n/)
  if (!lines[0]?.startsWith('untrusted comment:')) throw new Error(`${what} is not a minisign ${what} in Tauri's base64 format`)
  return lines
}

function ed25519Key(raw) {
  return createPublicKey({ key: { kty: 'OKP', crv: 'Ed25519', x: raw.toString('base64url') }, format: 'jwk' })
}

export function parsePublicKey(pubkeyBase64) {
  const lines = fromTauriBase64(pubkeyBase64, 'public key')
  const bytes = Buffer.from(lines[1] ?? '', 'base64')
  if (bytes.length !== 42 || bytes.subarray(0, 2).toString('latin1') !== 'Ed') throw new Error('Unsupported public key format')
  return { keyId: bytes.subarray(2, 10), key: ed25519Key(bytes.subarray(10, 42)) }
}

/**
 * Verifies `data` against a Tauri updater signature (the contents of a .sig file) and public
 * key. Returns the trusted comment; throws when anything does not match.
 */
export function verifyMinisign(data, signatureBase64, pubkeyBase64) {
  const publicKey = parsePublicKey(pubkeyBase64)
  const lines = fromTauriBase64(signatureBase64, 'signature')
  const signature = Buffer.from(lines[1] ?? '', 'base64')
  if (signature.length !== 74) throw new Error('Malformed signature line')
  const algorithm = signature.subarray(0, 2).toString('latin1')
  const keyId = signature.subarray(2, 10)
  const sig = signature.subarray(10, 74)
  if (!keyId.equals(publicKey.keyId)) {
    throw new Error(`Key id mismatch: signed by ${Buffer.from(keyId).reverse().toString('hex').toUpperCase()}, expected ${Buffer.from(publicKey.keyId).reverse().toString('hex').toUpperCase()}`)
  }
  let message
  if (algorithm === 'ED') message = createHash('blake2b512').update(data).digest()
  else if (algorithm === 'Ed') message = data
  else throw new Error(`Unsupported signature algorithm "${algorithm}"`)
  if (!verify(null, message, publicKey.key, sig)) throw new Error('The signature does not match the file')

  const prefix = 'trusted comment: '
  if (!lines[2]?.startsWith(prefix)) throw new Error('Missing trusted comment')
  const trusted = lines[2].slice(prefix.length)
  const globalSignature = Buffer.from(lines[3] ?? '', 'base64')
  if (globalSignature.length !== 64) throw new Error('Malformed global signature')
  if (!verify(null, Buffer.concat([sig, Buffer.from(trusted, 'utf8')]), publicKey.key, globalSignature)) {
    throw new Error('The trusted comment signature does not match')
  }
  return trusted
}

/** True when a trusted comment binds the signature to `version` (as `tauri build` writes it). */
export function bindsVersion(trusted, version) {
  return trusted.split('\t').includes(`version:${version}`)
}

function main(argv) {
  const positional = []
  let version
  let config = 'src-tauri/tauri.conf.json'
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === '--version') version = argv[++i]
    else if (argv[i] === '--config') config = argv[++i]
    else positional.push(argv[i])
  }
  const [file, signaturePath] = positional
  if (!file || !signaturePath) throw new Error('Usage: verify-minisign.mjs <file> <signature|-> [--version X.Y.Z] [--config tauri.conf.json]')
  const pubkey = JSON.parse(readFileSync(config, 'utf8'))?.plugins?.updater?.pubkey
  if (!pubkey) throw new Error(`No plugins.updater.pubkey in ${config}`)
  const signature = signaturePath === '-' ? process.env.SIGNATURE : readFileSync(signaturePath, 'utf8')
  if (!signature) throw new Error('No signature given')
  const trusted = verifyMinisign(readFileSync(file), signature, pubkey)
  if (version && !bindsVersion(trusted, version)) throw new Error(`The signature is not bound to version ${version} (trusted comment: "${trusted}")`)
  console.log(`Signature OK (${trusted})`)
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  try { main(process.argv.slice(2)) }
  catch (error) {
    console.error(`::error::${error.message}`)
    process.exit(1)
  }
}
