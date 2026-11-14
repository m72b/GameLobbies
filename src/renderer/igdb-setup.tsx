import { useEffect, useRef, useState } from 'react'
import type { IgdbCredentialStatus } from '../shared/types'

const CONSOLE_URL = 'https://dev.twitch.tv/console/apps'
const REGISTER_DOCS_URL = 'https://dev.twitch.tv/docs/authentication/register-app/'
const IGDB_DOCS_URL = 'https://api-docs.igdb.com/#account-creation'
const REDIRECT_URL = 'http://localhost'
const CATEGORY = 'Application Integration'

/** Pasted values often carry spaces or line breaks at either end. */
export const cleanPaste = (value: string) => value.trim()

/** A hint for a Client ID or Secret that looks wrong, or null. Twitch uses 30 lowercase letters and digits. */
export function credentialHint(label: string, value: string): { text: string; blocking: boolean } | null {
  const v = cleanPaste(value)
  if (!v) return null
  if (/\s/.test(v)) return { text: `The ${label} has a space or line break inside it. Copy it again from Twitch.`, blocking: true }
  if (!/^[a-z0-9]+$/.test(v)) return { text: `The ${label} should only have lowercase letters and numbers.`, blocking: false }
  if (v.length !== 30) return { text: `The ${label} is usually 30 characters long and this one has ${v.length}. Check that you copied all of it.`, blocking: false }
  return null
}

/** Turns the test command's message into a plain sentence. */
export function describeTest(ok: boolean, message: string): string {
  if (ok) return 'Connected. IGDB accepted your Client ID and Client Secret.'
  const m = message.toLowerCase()
  if (m.includes('rejected')) return 'Wrong Client ID or Client Secret. Check both against your Twitch application, or create a New Secret and paste it.'
  if (m.includes('could not reach')) return 'No internet connection to IGDB. Check your connection and try again.'
  if (m.includes('limiting requests')) return 'IGDB is rate-limiting requests right now. Wait a minute and try again.'
  if (m.includes('add your igdb')) return 'Save a Client ID and Client Secret first.'
  return message
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  useEffect(() => () => window.clearTimeout(timer.current), [])
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value)
      setCopied(true)
      window.clearTimeout(timer.current)
      timer.current = window.setTimeout(() => setCopied(false), 1800)
    } catch { setCopied(false) }
  }
  return <button type="button" className="secondary-button igdb-copy" onClick={() => void copy()} aria-label={`Copy ${label}`}>{copied ? 'Copied' : 'Copy'}</button>
}

export interface IgdbSetupProps {
  status: IgdbCredentialStatus
  clientId: string
  secret: string
  busy: boolean
  testing: boolean
  test: { ok: boolean; message: string } | null
  onClientId(value: string): void
  onSecret(value: string): void
  onSave(): void
  onClear(): void
  onTest(): void
}

export function IgdbSetup(p: IgdbSetupProps) {
  const configured = p.status.clientIdSet && p.status.clientSecretSet
  const [showSteps, setShowSteps] = useState<boolean | null>(null)
  const open = showSteps ?? !(configured && p.test?.ok !== false)
  const idHint = credentialHint('Client ID', p.clientId)
  const secretHint = credentialHint('Client Secret', p.secret)
  const blocked = !!(idHint?.blocking || secretHint?.blocking)
  const empty = !cleanPaste(p.clientId) && !cleanPaste(p.secret)
  const state = configured ? 'Credentials saved' : p.status.clientIdSet ? 'Client Secret missing' : p.status.clientSecretSet ? 'Client ID missing' : 'Not configured'

  return <>
    <p>IGDB supplies the game descriptions, genres, developers, publishers and ratings for the game page. It uses a free Twitch developer application that you create once.</p>
    {configured && <button type="button" className="link-button igdb-toggle" aria-expanded={open} aria-controls="igdb-walkthrough" onClick={() => setShowSteps(!open)}>{open ? 'Hide setup steps' : 'Show setup steps'}</button>}
    {open && <ol className="igdb-steps igdb-walkthrough" id="igdb-walkthrough">
      <li>
        <b>Sign in and turn on two-factor authentication.</b> Twitch requires a verified email and two-factor authentication on your account before it lets you register an application. Turn it on under Security and Privacy in your Twitch settings, then reload the console.
        <div className="igdb-step-actions"><a className="secondary-button" href={CONSOLE_URL}>Open the Twitch developer console</a></div>
      </li>
      <li>
        <b>Register your application.</b> On the Applications tab choose "Register Your Application" and fill in:
        <dl className="igdb-values">
          <dt>Name</dt><dd>Any name no one else uses, for example "Game Lobby" and your name.</dd>
          <dt>OAuth Redirect URLs</dt><dd><code>{REDIRECT_URL}</code><CopyButton value={REDIRECT_URL} label="redirect URL" /> then choose Add.</dd>
          <dt>Category</dt><dd><code>{CATEGORY}</code><CopyButton value={CATEGORY} label="category name" /></dd>
          <dt>Client Type</dt><dd>Confidential (if Twitch asks for one).</dd>
        </dl>
        Tick "I'm not a robot" and choose Create.
      </li>
      <li><b>Copy the Client ID.</b> Under Developer Applications choose Manage next to your application and copy the Client ID.</li>
      <li><b>Create the Client Secret.</b> Choose New Secret and copy it straight away. Twitch shows it only once, and a new secret replaces the old one.</li>
      <li><b>Paste both below and save</b>, then choose Test connection. They are encrypted by Windows and are not shown again.</li>
    </ol>}
    {open && <div className="igdb-links igdb-doc-links"><a className="external-link" href={REGISTER_DOCS_URL}>Twitch: registering an app</a><a className="external-link" href={IGDB_DOCS_URL}>IGDB account setup guide</a></div>}
    <form className="igdb-form" onSubmit={event => { event.preventDefault(); if (!empty && !blocked) p.onSave() }}>
      <label className="field-label">Client ID
        <input type="text" value={p.clientId} onChange={event => p.onClientId(event.target.value)} onBlur={() => p.onClientId(cleanPaste(p.clientId))} placeholder={p.status.clientIdSet ? 'Saved · enter a new Client ID to replace it' : 'Paste the Client ID'} autoComplete="off" spellCheck={false} maxLength={256} aria-invalid={!!idHint} aria-describedby={idHint ? 'igdb-id-hint' : undefined} />
      </label>
      {idHint && <div className="igdb-hint" id="igdb-id-hint" role="status">{idHint.text}</div>}
      <label className="field-label">Client Secret
        <input type="password" value={p.secret} onChange={event => p.onSecret(event.target.value)} onBlur={() => p.onSecret(cleanPaste(p.secret))} placeholder={p.status.clientSecretSet ? 'Saved · enter a new secret to replace it' : 'Paste the Client Secret'} autoComplete="new-password" spellCheck={false} maxLength={256} aria-invalid={!!secretHint} aria-describedby={secretHint ? 'igdb-secret-hint' : undefined} />
      </label>
      {secretHint && <div className="igdb-hint" id="igdb-secret-hint" role="status">{secretHint.text}</div>}
      <div className="settings-actions">
        <button className="secondary-button" disabled={p.busy || empty || blocked}>Save</button>
        {(p.status.clientIdSet || p.status.clientSecretSet) && <button type="button" className="secondary-button" disabled={p.busy} onClick={p.onClear}>Clear credentials</button>}
      </div>
    </form>
    <div className="sgdb-status-row">
      <span className="sgdb-status">{state}</span>
      <button type="button" className="secondary-button" disabled={!configured || p.testing} onClick={p.onTest}>{p.testing ? 'Testing…' : 'Test connection'}</button>
      {p.test && <span className={'sgdb-test ' + (p.test.ok ? 'ok' : 'fail')} role="status">{p.test.ok ? '✓' : '✗'} {describeTest(p.test.ok, p.test.message)}</span>}
    </div>
  </>
}
