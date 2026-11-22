import { useEffect, useMemo, useRef, useState } from 'react'
import { ArrowLeft, ArrowRight, Check, Sparkles } from 'lucide-react'
import type { AccountStore, SetupChoice, SetupInfo, SetupStore, SetupStoreInfo } from '../shared/types'
import { choiceLabel, choicesFor, defaultChoice, setupSteps, signInStores, type SetupStep } from '../shared/accounts'
import { StoreIcon } from './store-icons'
import { LibraryFoldersSection, useLocalScan } from './library-settings'
import { ApiKeysSection, type ApiKeys } from './api-keys'
import { SteamOwnedSetup } from './steam-owned-setup'
import { SignInNote, StoreSignIn, type Accounts } from './accounts'
import { GlMark } from './gl-mark'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

const stepTitles: Record<SetupStep, string> = {
  welcome: 'Welcome', stores: 'Your stores', choices: 'How to use each store', signin: 'Sign in',
  folders: 'Library folders', keys: 'Optional keys', done: 'All set'
}

const iconName: Record<SetupStore, string> = { steam: 'Steam', epic: 'Epic Games', gog: 'GOG', xbox: 'Xbox', battlenet: 'Battle.net', ea: 'EA app', ubisoft: 'Ubisoft', itch: 'itch.io' }

/**
 * First-time setup, shown inside the main window in place of the library: Welcome → stores →
 * a choice per store → Steam sign-in and key (when chosen) → library folders → optional keys →
 * Done (scans). Every step can be skipped; choices are saved through `setup.save`, which Rust
 * validates. `onDone(scan)` leaves the setup; `scan` asks App to scan the library.
 */
export function SetupView({ info, accounts, keys, say, onDone }: { info: SetupInfo; accounts: Accounts; keys: ApiKeys; say(text: string): void; onDone(scan: boolean): void }) {
  const stored = info.state?.choices ?? {}
  const [picked, setPicked] = useState<SetupStore[]>(() => info.stores.filter(store => stored[store.id] && stored[store.id] !== 'skip').map(store => store.id))
  const [choices, setChoices] = useState<Partial<Record<SetupStore, SetupChoice>>>(stored)
  const all = useMemo(() => choicesFor(info.stores, picked, choices), [info.stores, picked, choices])
  const steps = setupSteps(all)
  const [step, setStep] = useState<SetupStep>('welcome')
  const [saving, setSaving] = useState(false)
  const localScan = useLocalScan()
  const body = useRef<HTMLDivElement>(null)
  const index = Math.max(0, steps.indexOf(step))
  useEffect(() => { body.current?.scrollTo({ top: 0 }); body.current?.querySelector<HTMLElement>('h1')?.focus({ preventScroll: true }) }, [step])

  const saveChoices = async () => {
    try { await window.lobby.setup.save({ choices: all }); return true }
    catch (error) { say(errorText(error, 'Could not save your choices')); return false }
  }
  const go = async (to: SetupStep) => {
    if (step === 'choices' && !(await saveChoices())) return
    setStep(to)
  }
  const next = () => void go(steps[Math.min(steps.length - 1, index + 1)])
  const back = () => setStep(steps[Math.max(0, index - 1)])
  const skipAll = async () => {
    setSaving(true)
    try { await window.lobby.setup.save({ skipped: true }); onDone(false) }
    catch (error) { say(errorText(error, 'Could not skip the setup')) }
    finally { setSaving(false) }
  }
  const finish = async () => {
    setSaving(true)
    try { await window.lobby.setup.save({ choices: all, completed: true }); onDone(true) }
    catch (error) { say(errorText(error, 'Could not finish the setup')) }
    finally { setSaving(false) }
  }
  const toggle = (store: SetupStoreInfo) => setPicked(current => current.includes(store.id) ? current.filter(id => id !== store.id) : [...current, store.id])

  return <div className="settings-view setup-view">
    <header className="settings-head">
      <div className="brand"><GlMark size={12} className="gl-mark" />Set up GameLobbies</div>
      <ol className="setup-progress" aria-label="Setup steps">{steps.map((item, i) => <li key={item} className={i < index ? 'done' : i === index ? 'current' : ''} aria-current={i === index ? 'step' : undefined}><span>{stepTitles[item]}</span></li>)}</ol>
      <button type="button" className="link-button setup-skip" disabled={saving} onClick={() => void skipAll()}>Skip setup</button>
    </header>
    <div className="setup-body" ref={body}>
      <main className="setup-content" key={step}>
        <div className="eyebrow">STEP {index + 1} OF {steps.length}</div>
        <h1 tabIndex={-1}>{stepTitles[step]}</h1>

        {step === 'welcome' && <>
          <p className="settings-description">GameLobbies puts the games from all your stores and folders in one library. It finds installed games on its own by reading each launcher's files on this PC; no account is needed for that.</p>
          <p className="muted-text">The next steps let you choose your stores, sign in to Steam, GOG or Battle.net to also list the games you own but haven't installed, add game folders, and add optional keys for cover art and game details. Every step can be skipped, and everything can be changed later in Settings.</p>
        </>}

        {step === 'stores' && <>
          <p className="settings-description">Which stores do you use? Pick all that apply.</p>
          <div className="setup-stores" role="group" aria-label="Stores">
            {info.stores.map(store => <button type="button" key={store.id} className={'setup-store' + (picked.includes(store.id) ? ' picked' : '')} aria-pressed={picked.includes(store.id)} onClick={() => toggle(store)}>
              <span className="store-badge small"><StoreIcon store={iconName[store.id]} /></span><b>{store.name}</b>{picked.includes(store.id) && <Check size={14} className="setup-check" />}
            </button>)}
          </div>
          <p className="muted-text">Amazon Games, emulators and other launchers are still found on this PC; they need no setup.</p>
        </>}

        {step === 'choices' && <>
          {picked.length === 0 && <p className="settings-description">No stores picked. Installed games are still found on this PC; go on to add folders and keys, or go back to pick stores.</p>}
          {info.stores.filter(store => picked.includes(store.id)).map(store => {
            const options: SetupChoice[] = store.signIn ? ['signin', 'installed', 'skip'] : ['installed', 'skip']
            const current = all[store.id] ?? defaultChoice(store)
            return <div className="settings-card setup-choice" key={store.id}>
              <div className="setup-choice-head"><span className="store-badge small"><StoreIcon store={iconName[store.id]} /></span><b>{store.name}</b>{!store.signIn && store.id !== 'ea' && store.id !== 'ubisoft' && store.id !== 'itch' && <span className="coming-soon">SIGN-IN COMING SOON</span>}</div>
              <p className="muted-text">{store.note}</p>
              <div className="chip-group" role="radiogroup" aria-label={'How to use ' + store.name}>
                {options.map(option => <button type="button" key={option} role="radio" aria-checked={current === option} className={'chip ' + (current === option ? 'active' : '')} onClick={() => setChoices(value => ({ ...value, [store.id]: option }))}>{option === 'signin' ? (store.id === 'steam' ? 'Sign in through Steam' : 'Sign in to ' + store.name) : choiceLabel[option]}</button>)}
              </div>
            </div>
          })}
        </>}

        {step === 'signin' && <>
          <p className="settings-description">Sign in to the stores you chose. Each sign-in lists the games you own but haven't installed; skip any you like.</p>
          {signInStores(info.stores, all).map(store => {
            const status = accounts.get(store.id as AccountStore)
            if (!status) return null
            const steamStore = store.id === 'steam'
            return <div className="settings-card setup-choice" key={store.id}>
              <div className="setup-choice-head"><span className="store-badge small"><StoreIcon store={iconName[store.id]} /></span><b>{steamStore ? 'Sign in through Steam' : 'Sign in to ' + store.name}</b></div>
              {status.signedIn && !status.signingIn
                ? <p className="setup-ok" role="status"><Check size={14} /> Signed in to {store.name}.</p>
                : <p className="muted-text">{steamStore
                  ? 'Your web browser opens steamcommunity.com. Sign in there (approve with Steam Guard as usual); when Steam confirms, the tab says so and GameLobbies comes back to the front.'
                  : store.id === 'gog'
                    ? "Your web browser opens GOG's sign-in. Sign in any way GOG offers; then copy the address GOG ends on and paste it below."
                    : 'A GameLobbies window opens with ' + store.name + "'s sign-in page. Sign in there; when the store's account page appears, the window closes by itself. If it never finishes, close it and choose Installed files only."}</p>}
              {status.notice && !status.notice.ok && <p className="key-row-problem" role="status">✗ {status.notice.message}</p>}
              <StoreSignIn accounts={accounts} status={status} />
              <SignInNote store={status.store} />
              {steamStore && <div className="setup-key">
                <h3>Then add your Steam Web API key</h3>
                <SteamOwnedSetup status={keys.steam.status} busy={keys.steam.busy} error={keys.steam.error} onSave={keys.steam.save} onRemove={keys.steam.remove} />
              </div>}
            </div>
          })}
        </>}

        {step === 'folders' && <>
          <p className="settings-description">Add folders with PC games, ROMs or emulators, and GameLobbies scans them. Optional: skip this if your games all come from stores.</p>
          <LibraryFoldersSection scan={localScan} onOpenReview={() => say('Review what the scan found later in Settings → Review.')} say={say} />
        </>}

        {step === 'keys' && <>
          <p className="settings-description">Optional free keys for cover art and game details. GameLobbies works without them.</p>
          <ApiKeysSection keys={keys} />
        </>}

        {step === 'done' && <>
          <p className="settings-description"><Sparkles size={15} className="setup-sparkle" /> GameLobbies now scans your stores and folders{accounts.statuses.some(s => s.signedIn && s.mode === 'account') ? ' and asks your signed-in stores for the games you own' : ''}.</p>
          <p className="muted-text">Change any of this later: stores and accounts in Settings → Accounts and Libraries, folders in Library folders, keys in API keys. Settings → Accounts also has Run setup again.</p>
        </>}
      </main>
    </div>
    <footer className="setup-foot">
      {index > 0 ? <button type="button" className="secondary-button" disabled={saving} onClick={back}><ArrowLeft size={14} /> Back</button> : <span />}
      <span className="setup-foot-space" />
      {step !== 'done' && step !== 'welcome' && <button type="button" className="link-button" disabled={saving} onClick={next}>Skip this step</button>}
      {step === 'done'
        ? <button type="button" className="primary-button" disabled={saving} onClick={() => void finish()}>{saving ? 'Finishing…' : 'Finish and scan'}</button>
        : <button type="button" className="primary-button" disabled={saving} onClick={next}>{step === 'welcome' ? 'Get started' : 'Next'} <ArrowRight size={14} /></button>}
    </footer>
  </div>
}
