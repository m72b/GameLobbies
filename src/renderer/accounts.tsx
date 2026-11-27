import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import { Check, LogIn, RefreshCw, ShieldCheck, X } from 'lucide-react'
import type { AccountMode, AccountStatus, AccountStore } from '../shared/types'
import { accountLine, canSync } from '../shared/accounts'
import { StoreIcon } from './store-icons'
import { useKeysUi } from './api-keys'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

const iconName: Record<AccountStore, string> = { steam: 'Steam', gog: 'GOG', battlenet: 'Battle.net', xbox: 'Xbox', epic: 'Epic Games' }

/**
 * Store account statuses for the whole window (kept by App), with the calls that change them.
 * The backend sends `accounts:changed` after a sign-in ends; a new notice is also shown as a toast.
 * `onChanged` runs after every change (App reloads the API key flags: a Steam sign-in saves the SteamID).
 */
export function useAccounts(say: (text: string) => void, onChanged: () => void) {
  const [statuses, setStatuses] = useState<AccountStatus[]>([])
  const [busy, setBusy] = useState<AccountStore | null>(null)
  const sayRef = useRef(say); sayRef.current = say
  const changedRef = useRef(onChanged); changedRef.current = onChanged
  const seen = useRef(new Map<AccountStore, number>())
  const take = useCallback((next: AccountStatus[], announce: boolean) => {
    for (const status of next) {
      const at = status.notice?.at ?? 0
      const before = seen.current.get(status.store)
      seen.current.set(status.store, at)
      if (announce && status.notice && before !== undefined && at > before) sayRef.current(status.notice.message)
    }
    setStatuses(next)
    changedRef.current()
  }, [])
  useEffect(() => {
    void window.lobby.accounts.status().then(next => take(next, false)).catch(() => undefined)
    return window.lobby.accounts.onChanged(next => take(next, true))
  }, [take])
  const run = async (store: AccountStore, action: () => Promise<AccountStatus[]>, fallback: string) => {
    setBusy(store)
    try { take(await action(), true); return true }
    catch (error) { sayRef.current(errorText(error, fallback)); return false }
    finally { setBusy(null) }
  }
  return {
    statuses, busy,
    get: (store: AccountStore) => statuses.find(status => status.store === store),
    signIn: (store: AccountStore) => run(store, () => window.lobby.accounts.signIn(store), 'Could not start the sign-in'),
    cancel: (store: AccountStore) => run(store, () => window.lobby.accounts.cancelSignIn(), 'Could not cancel the sign-in'),
    signOut: (store: AccountStore) => run(store, () => window.lobby.accounts.signOut(store), 'Could not sign out'),
    setMode: (store: AccountStore, mode: AccountMode) => run(store, () => window.lobby.accounts.setMode(store, mode), 'Could not change the mode'),
    sync: (store: AccountStore) => run(store, () => window.lobby.accounts.sync(store), 'Sync failed'),
    submitCode: (store: AccountStore, code: string) => run(store, () => window.lobby.accounts.submitCode(store, code), 'The sign-in was not accepted')
  }
}
export type Accounts = ReturnType<typeof useAccounts>

/** The note sign-in places show: where each sign-in happens and where the password goes. */
const STEAM_NOTE = "Steam sign-in happens in your own web browser, on steamcommunity.com. Your password and Steam Guard approval go only to Steam; GameLobbies only receives your SteamID64 back, after Steam confirms it. You can close the browser tab afterwards."
const GOG_NOTE = "GOG sign-in happens in your own web browser, on gog.com, with any way GOG offers (e-mail, Google, Discord and others). When GOG shows that you are signed in, copy the address from the browser's address bar (it starts with https://embed.gog.com/on_login_success) and paste it into GameLobbies. Unofficial: GameLobbies signs in with GOG Galaxy's public app id, as other open-source launchers do, and GOG could stop that at any time. GameLobbies keeps GOG's sign-in tokens encrypted by Windows; your password goes only to GOG."
const WINDOW_NOTE = "Battle.net signs in in GameLobbies's own private window showing Battle.net's real sign-in page. Your password goes only to Battle.net; GameLobbies keeps only its session, encrypted by Windows. The account is read the way Battle.net's own website reads it, not through an official interface, so it can stop working; installed games are found either way."

export function SignInNote({ store }: { store?: AccountStore }) {
  const text = store === 'steam' ? STEAM_NOTE : store === 'gog' ? GOG_NOTE : store ? WINDOW_NOTE : [STEAM_NOTE, GOG_NOTE, WINDOW_NOTE].join(' ')
  return <p className="account-note"><ShieldCheck size={16} aria-hidden="true" /><span>{text}</span></p>
}

/** A store's sign-in with its states (Settings → Accounts and the first-time setup). */
export function StoreSignIn({ accounts, status }: { accounts: Accounts; status: AccountStatus }) {
  const busy = accounts.busy === status.store
  const steam = status.store === 'steam'
  const gog = status.store === 'gog'
  const [pasted, setPasted] = useState('')
  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!pasted.trim()) return
    void accounts.submitCode(status.store, pasted).then(done => { if (done) setPasted('') })
  }
  if (status.signingIn && gog) return <form className="account-paste" onSubmit={submit}>
    <label className="field-label">Address GOG ends on after you sign in
      <input type="text" value={pasted} onChange={event => setPasted(event.target.value)} placeholder="https://embed.gog.com/on_login_success?origin=client&code=…" autoComplete="off" spellCheck={false} maxLength={4096} aria-describedby="gog-paste-help" />
    </label>
    <small id="gog-paste-help" className="muted-text">Sign in on the GOG page your browser opened. GOG then shows a page saying you're signed in: copy its address from the address bar and paste it here.</small>
    <div className="account-actions">
      <button type="submit" className="primary-button" disabled={busy || !pasted.trim()}>Continue</button>
      <button type="button" className="secondary-button" disabled={busy} onClick={() => void accounts.signIn('gog')}>Open GOG's page again</button>
      <button type="button" className="secondary-button" disabled={busy} onClick={() => void accounts.cancel('gog')}>Cancel sign-in</button>
    </div>
  </form>
  if (status.signingIn) return <div className="account-actions"><span className="account-waiting" role="status">{steam ? 'Continue in your browser…' : 'Finish signing in in the ' + status.name + ' window…'}</span><button type="button" className="secondary-button" disabled={busy} onClick={() => void accounts.cancel(status.store)}>Cancel sign-in</button></div>
  const again = status.signedIn
  const label = again ? 'Sign in again' : steam ? 'Sign in through Steam' : gog ? 'Sign in to GOG in your browser' : 'Sign in to ' + status.name
  return <div className="account-actions"><button type="button" className={again ? 'secondary-button' : 'primary-button'} disabled={busy} onClick={() => void accounts.signIn(status.store)}><LogIn size={16} /> {label}</button></div>
}

function AccountRow({ status, accounts }: { status: AccountStatus; accounts: Accounts }) {
  const { openSheet } = useKeysUi()
  // 'out': confirm signing out; 'installed': confirm switching to installed files only (both remove games).
  const [confirming, setConfirming] = useState<'out' | 'installed' | null>(null)
  const line = accountLine(status)
  const busy = accounts.busy === status.store
  return <div className="account-row">
    <span className="store-badge small"><StoreIcon store={iconName[status.store]} /></span>
    <div className="account-main">
      <div className="key-row-title"><b>{status.name}</b><span className={'key-pill ' + (line.state === 'ok' ? 'set' : line.state === 'partial' ? 'partial' : '')}>{line.pill}</span></div>
      <small>{line.label}</small>
      {status.last && status.signedIn && <small className={status.last.ok ? 'account-last' : 'key-row-problem'}>{status.last.ok ? <Check size={14} strokeWidth={1.5} /> : <X size={14} strokeWidth={1.5} />} {status.last.message} · {new Date(status.last.checkedAt).toLocaleString()}</small>}
      {status.notice && !status.notice.ok && <small className="key-row-problem" role="status"><X size={14} strokeWidth={1.5} /> {status.notice.message}</small>}
      {status.store === 'gog' && status.signedIn && status.notice?.ok && status.notice.message.startsWith('Signed in to GOG as ') && <small className="account-last" role="status">{status.notice.message} <button type="button" className="link-button" onClick={() => setConfirming('out')}>Not you? Sign out</button></small>}
      {status.available && <>
        {(!status.signedIn || status.last?.needsSignIn) && <StoreSignIn accounts={accounts} status={status} />}
        {status.signedIn && <div className="account-actions">
          <label className="account-mode">Use<select value={status.mode} disabled={busy || status.signingIn} aria-label={'How GameLobbies uses ' + status.name} onChange={event => { const mode = event.target.value as AccountMode; if (mode === 'installed') setConfirming('installed'); else { setConfirming(null); void accounts.setMode(status.store, mode) } }}>
            <option value="account">Account: owned games and playtime</option>
            <option value="installed">Installed files only</option>
          </select></label>
          {status.store === 'steam' && !status.keySet && <button type="button" className="primary-button" onClick={() => openSheet('steam')}>Add API key</button>}
          <button type="button" className="secondary-button" disabled={busy || !canSync(status)} onClick={() => void accounts.sync(status.store)}><RefreshCw size={16} className={busy ? 'spin' : ''} /> Sync now</button>
          {confirming === 'out'
            ? <span className="confirm-group"><span>Sign out and remove the not-installed games it added?</span><button type="button" className="secondary-button danger" disabled={busy} onClick={() => { setConfirming(null); void accounts.signOut(status.store) }}>Sign out</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
            : confirming === 'installed'
              ? <span className="confirm-group"><span>Show installed games only and remove the not-installed games it added? Games you played, rated or customized stay.</span><button type="button" className="secondary-button danger" disabled={busy} onClick={() => { setConfirming(null); void accounts.setMode(status.store, 'installed') }}>Installed only</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
              : <button type="button" className="secondary-button" disabled={busy} onClick={() => setConfirming('out')}>Sign out</button>}
        </div>}

      </>}
    </div>
  </div>
}

/** Settings → Accounts: every store account, its status, sign in / out, mode and sync. */
export function AccountsSection({ accounts, onRunSetup }: { accounts: Accounts; onRunSetup(): void }) {
  return <>
    <div className="settings-card">
      <div className="card-heading"><div><h3>Store accounts</h3><p>Optional. Without an account GameLobbies lists the games installed on this PC. Signed in, it also lists the games you own but haven't installed: Steam (with a free Web API key, and Steam's playtime), GOG (unofficial, with GOG's playtime) and Battle.net. Signing out removes the not-installed games it added; games you played, rated or customized stay.</p></div></div>
      {accounts.statuses.map(status => <AccountRow key={status.store} status={status} accounts={accounts} />)}
      <SignInNote />
    </div>
    <div className="settings-card">
      <h3>First-time setup</h3>
      <p className="muted-text">Walk through stores, library folders and optional keys again. Nothing changes until you choose it.</p>
      <div className="settings-actions"><button type="button" className="secondary-button" onClick={onRunSetup}>Run setup again</button></div>
    </div>
  </>
}
