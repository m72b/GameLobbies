import { createContext, useCallback, useContext, useEffect, useRef, useState } from 'react'
import { KeyRound } from 'lucide-react'
import type { IgdbCredentialStatus, SteamOwnedStatus } from '../shared/types'
import { igdbSummary, keyInfo, keyOrder, pillWord, sgdbSummary, steamSummary, type KeyId, type KeySummary } from '../shared/api-keys'
import { IgdbSetup, cleanPaste } from './igdb-setup'
import { SteamOwnedSetup } from './steam-owned-setup'
import { Sheet } from './sheet'

const SGDB_URL = 'https://www.steamgriddb.com/profile/preferences/api'
const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

type TestResult = { ok: boolean; message: string }

/**
 * The state of all optional API keys, with the calls that change them. There is one copy for the
 * whole window (kept by App), so the API keys tab, the sheet and the status lines never disagree.
 * The renderer only ever learns whether a key is set, never its value.
 */
export function useApiKeys(say: (text: string) => void) {
  const [sgdbSet, setSgdbSet] = useState(false)
  const [sgdbBusy, setSgdbBusy] = useState(false)
  const [sgdbTesting, setSgdbTesting] = useState(false)
  const [sgdbTest, setSgdbTest] = useState<TestResult | null>(null)
  const [igdbStatus, setIgdbStatus] = useState<IgdbCredentialStatus>({ clientIdSet: false, clientSecretSet: false })
  const [igdbBusy, setIgdbBusy] = useState(false)
  const [igdbTesting, setIgdbTesting] = useState(false)
  const [igdbTest, setIgdbTest] = useState<TestResult | null>(null)
  const [steamStatus, setSteamStatus] = useState<SteamOwnedStatus>({ keySet: false, steamIdSet: false, last: null })
  const [steamBusy, setSteamBusy] = useState(false)
  const [steamError, setSteamError] = useState('')
  const sayRef = useRef(say)
  sayRef.current = say

  const reload = useCallback(() => Promise.all([
    window.lobby.steamGridDb.status().then(result => setSgdbSet(result.keySet)).catch(() => undefined),
    window.lobby.igdb.status().then(setIgdbStatus).catch(() => undefined),
    window.lobby.steamOwned.status().then(setSteamStatus).catch(() => undefined)
  ]).then(() => undefined), [])
  useEffect(() => {
    void reload()
    // A scan can change the Steam import's last result.
    return window.lobby.library.onChanged(() => { void window.lobby.steamOwned.status().then(setSteamStatus).catch(() => undefined) })
  }, [reload])

  const saveSgdb = async (key: string) => {
    setSgdbBusy(true)
    try {
      const result = await window.lobby.steamGridDb.saveKey(key)
      setSgdbSet(result.keySet); setSgdbTest(null)
      sayRef.current(result.keySet ? 'SteamGridDB key saved' : 'SteamGridDB key removed')
      return true
    } catch (error) { sayRef.current(errorText(error, 'Could not save the SteamGridDB key')); return false }
    finally { setSgdbBusy(false) }
  }
  const testSgdb = async () => {
    setSgdbTesting(true)
    try { const result = await window.lobby.steamGridDb.testKey(); setSgdbTest(result); sayRef.current(result.message) }
    catch (error) { const text = errorText(error, 'Could not test the SteamGridDB key'); setSgdbTest({ ok: false, message: text }); sayRef.current(text) }
    finally { setSgdbTesting(false) }
  }
  const saveIgdb = async (clientId: string, secret: string) => {
    setIgdbBusy(true)
    try {
      const result = await window.lobby.igdb.saveCredentials(cleanPaste(clientId), cleanPaste(secret))
      setIgdbStatus(result); setIgdbTest(null)
      sayRef.current(result.clientIdSet && result.clientSecretSet ? 'IGDB credentials saved' : 'Saved. Enter both the Client ID and the Client Secret.')
      return true
    } catch (error) { sayRef.current(errorText(error, 'Could not save the IGDB credentials')); return false }
    finally { setIgdbBusy(false) }
  }
  const clearIgdb = async () => {
    setIgdbBusy(true)
    try { setIgdbStatus(await window.lobby.igdb.clearCredentials()); setIgdbTest(null); sayRef.current('IGDB credentials removed') }
    catch (error) { sayRef.current(errorText(error, 'Could not remove the IGDB credentials')) }
    finally { setIgdbBusy(false) }
  }
  const testIgdb = async () => {
    setIgdbTesting(true)
    try { const result = await window.lobby.igdb.test(); setIgdbTest(result); sayRef.current(result.message) }
    catch (error) { const text = errorText(error, 'Could not test the IGDB connection'); setIgdbTest({ ok: false, message: text }); sayRef.current(text) }
    finally { setIgdbTesting(false) }
  }
  const saveSteam = async (key: string, steamId: string) => {
    setSteamBusy(true); setSteamError('')
    try {
      const next = await window.lobby.steamOwned.save(cleanPaste(key), cleanPaste(steamId))
      setSteamStatus(next)
      sayRef.current(next.keySet && next.steamIdSet ? (next.last?.message ?? 'Steam key saved') : 'Saved. Enter both the API key and the SteamID64.')
      return true
    } catch (caught) { setSteamError(errorText(caught, 'Could not save the Steam key')); return false }
    finally { setSteamBusy(false) }
  }
  const removeSteam = async () => {
    setSteamBusy(true); setSteamError('')
    try { setSteamStatus(await window.lobby.steamOwned.remove()); sayRef.current('Steam key removed') }
    catch (caught) { setSteamError(errorText(caught, 'Could not remove the Steam key')) }
    finally { setSteamBusy(false) }
  }

  const summary: Record<KeyId, KeySummary> = { steamgriddb: sgdbSummary(sgdbSet), igdb: igdbSummary(igdbStatus), steam: steamSummary(steamStatus) }
  /** The last thing that went wrong with a key (a failed test or import), if anything. */
  const problem: Record<KeyId, string> = {
    steamgriddb: sgdbTest && !sgdbTest.ok ? sgdbTest.message : '',
    igdb: igdbTest && !igdbTest.ok ? igdbTest.message : '',
    steam: steamError || (steamStatus.last && !steamStatus.last.ok ? steamStatus.last.message : '')
  }
  return {
    reload, summary, problem,
    sgdb: { keySet: sgdbSet, busy: sgdbBusy, testing: sgdbTesting, test: sgdbTest, save: saveSgdb, runTest: testSgdb },
    igdb: { status: igdbStatus, configured: igdbStatus.clientIdSet && igdbStatus.clientSecretSet, busy: igdbBusy, testing: igdbTesting, test: igdbTest, save: saveIgdb, clear: clearIgdb, runTest: testIgdb },
    steam: { status: steamStatus, busy: steamBusy, error: steamError, save: saveSteam, remove: removeSteam },
    /** Removes a key's saved value(s). */
    remove: async (id: KeyId) => { if (id === 'steamgriddb') await saveSgdb(''); else if (id === 'igdb') await clearIgdb(); else await removeSteam() }
  }
}
export type ApiKeys = ReturnType<typeof useApiKeys>

interface KeysUi { keys: ApiKeys | null; /** Opens the sheet with every key (optionally scrolled to one). */ openSheet(which?: KeyId): void; /** Opens Settings on the API keys tab. */ openTab(): void }
export const KeysUiContext = createContext<KeysUi>({ keys: null, openSheet: () => void window.lobby.settings.open('api-keys'), openTab: () => void window.lobby.settings.open('api-keys') })
export const useKeysUi = () => useContext(KeysUiContext)

/** A one-line status for a key with a link to the API keys tab; used where a full form used to be. */
export function KeyStatusLine({ id, keys, note }: { id: KeyId; keys: ApiKeys; note?: string }) {
  const { openTab } = useKeysUi()
  const { state, label } = keys.summary[id]
  return <div className="key-status-line">
    <span className={'key-pill ' + state}>{pillWord(state)}</span>
    <span><b>{keyInfo[id].name}</b> · {label}{note ? ' · ' + note : ''}</span>
    <button type="button" className="link-button" onClick={openTab}>Manage in API keys</button>
  </div>
}

/** Settings → API keys: every optional key, what it unlocks, its status, and Add / Change / Remove. */
export function ApiKeysSection({ keys }: { keys: ApiKeys }) {
  const { openSheet } = useKeysUi()
  const [confirming, setConfirming] = useState<KeyId | null>(null)
  return <>
    <div className="settings-card">
      <div className="card-heading"><div><h3>Optional keys</h3><p>Game Lobby works without any of these. Each one unlocks extra information or art from a free service. Keys are encrypted by Windows, are never shown again after saving, and never leave this PC except to the service they belong to.</p></div><button className="primary-button" onClick={() => openSheet()}><KeyRound size={15} /> Add keys</button></div>
      {keyOrder.map(id => {
        const { state, label } = keys.summary[id]
        const saved = state !== 'unset'
        const problem = keys.problem[id]
        return <div className="key-row" key={id} id={'key-row-' + id}>
          <div className="key-row-main">
            <div className="key-row-title"><b>{keyInfo[id].name}</b><span className={'key-pill ' + state}>{pillWord(state)}</span></div>
            <small>{keyInfo[id].unlocks}</small>
            <small className="key-row-status">{label}</small>
            {problem && <small className="key-row-problem" role="status">✗ {problem}</small>}
          </div>
          <div className="key-row-actions">
            {confirming === id
              ? <><span className="key-confirm">Remove this key?</span><button type="button" className="secondary-button danger" onClick={() => { setConfirming(null); void keys.remove(id) }}>Remove</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></>
              : <><button type="button" className="secondary-button" onClick={() => openSheet(id)}>{saved ? 'Change' : 'Add'}</button>{saved && <button type="button" className="secondary-button" onClick={() => setConfirming(id)}>Remove</button>}</>}
          </div>
        </div>
      })}
    </div>
  </>
}

function SgdbForm({ keys }: { keys: ApiKeys }) {
  const [value, setValue] = useState('')
  const { sgdb } = keys
  return <>
    <p className="muted-text">Sign in at steamgriddb.com, then open Profile → Preferences → API and copy your key. <a className="external-link inline" href={SGDB_URL}>Get an API key</a></p>
    <form className="sheet-form" onSubmit={event => { event.preventDefault(); const key = cleanPaste(value); if (key) void sgdb.save(key).then(saved => { if (saved) setValue('') }) }}>
      <label className="sheet-field">SteamGridDB API key
        <input type="password" value={value} onChange={event => setValue(event.target.value)} placeholder={sgdb.keySet ? 'Saved · enter a new key to replace it' : 'Paste your API key'} autoComplete="new-password" spellCheck={false} maxLength={256} />
      </label>
      <div className="settings-actions">
        <button className="secondary-button" disabled={sgdb.busy || !cleanPaste(value)}>Save</button>
        {sgdb.keySet && <button type="button" className="secondary-button" disabled={sgdb.busy} onClick={() => void sgdb.save('')}>Remove key</button>}
      </div>
    </form>
    <div className="sgdb-status-row" role="status">
      <span className="sgdb-status">{keys.summary.steamgriddb.label}</span>
      <button type="button" className="secondary-button" disabled={!sgdb.keySet || sgdb.testing} onClick={() => void sgdb.runTest()}>{sgdb.testing ? 'Testing…' : 'Test key'}</button>
      {sgdb.test && <span className={'sgdb-test ' + (sgdb.test.ok ? 'ok' : 'fail')}>{sgdb.test.ok ? '✓' : '✗'} {sgdb.test.message}</span>}
    </div>
  </>
}

function IgdbForm({ keys }: { keys: ApiKeys }) {
  const [clientId, setClientId] = useState('')
  const [secret, setSecret] = useState('')
  const { igdb } = keys
  return <IgdbSetup status={igdb.status} clientId={clientId} secret={secret} busy={igdb.busy} testing={igdb.testing} test={igdb.test}
    onClientId={setClientId} onSecret={setSecret}
    onSave={() => void igdb.save(clientId, secret).then(saved => { if (saved) { setClientId(''); setSecret('') } })}
    onClear={() => void igdb.clear()} onTest={() => void igdb.runTest()} />
}

/** The sheet with every optional key at once. `focus` scrolls to one of them. */
export function ApiKeysSheet({ open, focus, keys, onClose }: { open: boolean; focus?: KeyId; keys: ApiKeys; onClose(): void }) {
  const body = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!open || !focus) return
    const frame = requestAnimationFrame(() => {
      const card = body.current?.querySelector<HTMLElement>('#key-card-' + focus)
      card?.scrollIntoView({ block: 'start', behavior: 'smooth' })
      card?.querySelector<HTMLElement>('input')?.focus({ preventScroll: true })
    })
    return () => cancelAnimationFrame(frame)
  }, [open, focus])
  return <Sheet open={open} title="API keys" subtitle="All optional. Add the ones you want; each unlocks something extra." onClose={onClose}>
    <div ref={body} className="key-cards">
      {keyOrder.map(id => {
        const { state, label } = keys.summary[id]
        return <section className={'key-card' + (focus === id ? ' focus' : '')} key={id} id={'key-card-' + id} aria-label={keyInfo[id].name}>
          <div className="key-card-head"><h3>{keyInfo[id].name}</h3><span className={'key-pill ' + state} title={label}>{pillWord(state)}</span></div>
          <p className="key-unlocks">Unlocks: {keyInfo[id].unlocks}</p>
          {id === 'steamgriddb' && <SgdbForm keys={keys} />}
          {id === 'igdb' && <IgdbForm keys={keys} />}
          {id === 'steam' && <SteamOwnedSetup status={keys.steam.status} busy={keys.steam.busy} error={keys.steam.error} onSave={keys.steam.save} onRemove={keys.steam.remove} />}
        </section>
      })}
    </div>
  </Sheet>
}
