import { useState } from 'react'
import type { SteamOwnedStatus } from '../shared/types'
import { cleanPaste } from './igdb-setup'
import { steamSummary } from '../shared/api-keys'

const API_KEY_URL = 'https://steamcommunity.com/dev/apikey'
const PRIVACY_URL = 'https://steamcommunity.com/my/edit/settings'
const ACCOUNT_URL = 'https://store.steampowered.com/account/'

/** A hint for input that the backend will reject, or null. The backend checks again. */
export function steamKeyHint(value: string): string | null {
  const v = cleanPaste(value)
  if (!v) return null
  if (!/^[0-9a-fA-F]{32}$/.test(v)) return 'A Steam Web API key is 32 characters: digits and the letters A to F.'
  return null
}

export function steamIdHint(value: string): string | null {
  const v = cleanPaste(value)
  if (!v) return null
  if (!/^7656119\d{10}$/.test(v)) return 'A SteamID64 is 17 digits and starts with 7656119.'
  return null
}

/**
 * The form for the optional Steam Web API import of owned games that aren't installed (shown in
 * the API keys sheet). The key and SteamID are write-only: only whether each is saved is shown.
 * Saving and removing are done by the caller (`useApiKeys`), so every screen shows one state.
 */
export function SteamOwnedSetup({ status, busy, error, onSave, onRemove }: {
  status: SteamOwnedStatus
  busy: boolean
  error: string
  onSave(key: string, steamId: string): Promise<boolean>
  onRemove(): Promise<void>
}) {
  const [key, setKey] = useState('')
  const [steamId, setSteamId] = useState('')
  const [confirming, setConfirming] = useState(false)
  const keyHint = steamKeyHint(key)
  const idHint = steamIdHint(steamId)
  const empty = !cleanPaste(key) && !cleanPaste(steamId)
  const save = async () => { if (await onSave(cleanPaste(key), cleanPaste(steamId))) { setKey(''); setSteamId('') } }
  const remove = async () => { setConfirming(false); await onRemove() }
  return <div className="steam-owned">
    <p className="muted-text">With a free Steam Web API key and your SteamID64, GameLobbies asks Steam for the games you own and adds the ones that aren't installed, so they show under All. Without a key only installed games are listed. The key and SteamID are encrypted by Windows and are not shown again.</p>
    <ol className="igdb-steps">
      <li>Get a key at <a className="external-link" href={API_KEY_URL}>steamcommunity.com/dev/apikey</a>. Any domain name works, for example “localhost”.</li>
      <li>Find your SteamID64: on your <a className="external-link" href={ACCOUNT_URL}>Steam account page</a> it is shown as “Steam ID” under your account name. It is 17 digits and starts with 7656119.</li>
      <li>Your profile’s Game details must be Public in the <a className="external-link" href={PRIVACY_URL}>Steam privacy settings</a>.</li>
    </ol>
    <form className="igdb-form" onSubmit={event => { event.preventDefault(); if (!empty && !keyHint && !idHint) void save() }}>
      <label className="field-label">Steam Web API key
        <input type="password" value={key} onChange={event => setKey(event.target.value)} onBlur={() => setKey(cleanPaste(key))} placeholder={status.keySet ? 'Saved · enter a new key to replace it' : 'Paste your API key'} autoComplete="new-password" spellCheck={false} maxLength={256} aria-invalid={!!keyHint} aria-describedby={keyHint ? 'steam-key-hint' : undefined} />
      </label>
      {keyHint && <div className="igdb-hint" id="steam-key-hint" role="status">{keyHint}</div>}
      <label className="field-label">SteamID64
        <input type="text" inputMode="numeric" value={steamId} onChange={event => setSteamId(event.target.value)} onBlur={() => setSteamId(cleanPaste(steamId))} placeholder={status.steamIdSet ? 'Saved · enter a new SteamID64 to replace it' : '7656119…'} autoComplete="off" spellCheck={false} maxLength={64} aria-invalid={!!idHint} aria-describedby={idHint ? 'steam-id-hint' : undefined} />
      </label>
      {idHint && <div className="igdb-hint" id="steam-id-hint" role="status">{idHint}</div>}
      {error && <div className="form-error" role="alert">{error}</div>}
      <div className="settings-actions">
        <button className="secondary-button" disabled={busy || empty || !!keyHint || !!idHint}>{busy ? 'Working…' : 'Save'}</button>
        {(status.keySet || status.steamIdSet) && (confirming
          ? <span className="confirm-group"><span>Remove the key and the not-installed games it added?</span><button type="button" className="secondary-button danger" disabled={busy} onClick={() => void remove()}>Remove</button><button type="button" className="secondary-button" onClick={() => setConfirming(false)}>Cancel</button></span>
          : <button type="button" className="secondary-button" disabled={busy} onClick={() => setConfirming(true)}>Remove</button>)}
      </div>
    </form>
    <div className="sgdb-status-row" role="status">
      <span className="sgdb-status">{steamSummary(status).label}</span>
      {status.last && <span className={'sgdb-test ' + (status.last.ok ? 'ok' : 'fail')}>{status.last.ok ? '✓' : '✗'} {status.last.message}{status.last.checkedAt ? ' · ' + new Date(status.last.checkedAt).toLocaleString() : ''}</span>}
    </div>
  </div>
}
