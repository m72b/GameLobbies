import { useCallback, useEffect, useState } from 'react'
import { HealthCheckCard, HealthItem, healthLabels, healthTitle, isHealthItem, type HealthKind } from './health-items'
import type { IgnoredPath, ReviewItem, ReviewResolution } from '../shared/types'

const errorText = (error: unknown) => error instanceof Error ? error.message : typeof error === 'string' ? error : 'Could not apply this decision'

type ItemProps<K extends ReviewItem['kind']> = { item: Extract<ReviewItem, { kind: K }>; apply(resolution: ReviewResolution): void; busy: boolean }

function DuplicateItem({ item, apply, busy }: ItemProps<'duplicate'>) {
  const [linking, setLinking] = useState(false)
  const [target, setTarget] = useState(item.details.games[0]?.gameId ?? '')
  return <>
    <p className="muted-text">Same system and title found as different games. They are never merged automatically.</p>
    <ul className="review-games">{item.details.games.map(game => <li key={game.gameId}><b>{game.title}</b>{game.paths.map(path => <div className="root-path" key={path} title={path}>{path}</div>)}</li>)}</ul>
    {linking && <fieldset className="review-choices"><legend>Keep which game? The others are merged into it.</legend>
      {item.details.games.map(game => <label key={game.gameId}><input type="radio" name={'target-' + item.id} checked={target === game.gameId} onChange={() => setTarget(game.gameId)} /> {game.title}{game.paths[0] ? ' · ' + game.paths[0] : ''}</label>)}</fieldset>}
    <div className="settings-actions">
      {linking
        ? <><button type="button" className="primary-button" disabled={busy || !target} onClick={() => apply({ action: 'link', targetGameId: target })}>Link games</button><button type="button" className="secondary-button" onClick={() => setLinking(false)}>Cancel</button></>
        : <><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'keep-separate' })}>Keep separate</button><button type="button" className="secondary-button" disabled={busy} onClick={() => setLinking(true)}>Link…</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'dismiss' })}>Dismiss</button></>}
    </div>
  </>
}

function ExeItem({ item, apply, busy }: ItemProps<'ambiguous-exe'>) {
  const [exe, setExe] = useState('')
  return <>
    <p className="muted-text">No single executable stands out in this folder, so the game isn’t added yet. Choose the one that starts it.</p>
    <div className="root-path" title={item.details.folder}>{item.details.folder}</div>
    <fieldset className="review-choices"><legend>Executable for {item.details.title}</legend>
      {item.details.candidates.map(candidate => <label key={candidate}><input type="radio" name={'exe-' + item.id} checked={exe === candidate} onChange={() => setExe(candidate)} /> {candidate}</label>)}</fieldset>
    <div className="settings-actions"><button type="button" className="primary-button" disabled={busy || !exe} onClick={() => apply({ action: 'choose-exe', executable: exe })}>Use this executable</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'ignore', path: item.details.folder })}>Ignore folder</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'dismiss' })}>Dismiss</button></div>
  </>
}

function UncertainRomItem({ item, apply, busy }: ItemProps<'uncertain-rom'>) {
  const [emulator, setEmulator] = useState(item.details.emulators.length === 1 ? item.details.emulators[0].id : '')
  const systems = item.details.systems.map(system => system.name).join(', ')
  return <>
    <p className="muted-text">This file’s type ({systems}) is used by several systems, so Game Lobby can’t tell which emulator plays it. Choose one; you can change it later on the game’s page.</p>
    <div className="root-path" title={item.details.path}>{item.details.path}</div>
    {item.details.emulators.length === 0
      ? <p className="muted-text">None of your emulators runs {systems}. Add one (Settings → Emulators), then rescan.</p>
      : <fieldset className="review-choices"><legend>Emulator for {item.details.title}</legend>
        {item.details.emulators.map(option => <label key={option.id}><input type="radio" name={'emu-' + item.id} checked={emulator === option.id} onChange={() => setEmulator(option.id)} /> {option.name}{option.systems.length ? ' · ' + option.systems.map(id => item.details.systems.find(system => system.id === id)?.name ?? id).join(', ') : ''}</label>)}</fieldset>}
    <div className="settings-actions"><button type="button" className="primary-button" disabled={busy || !emulator} onClick={() => apply({ action: 'choose-emulator', emulatorId: emulator })}>Use this emulator</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'ignore', path: item.details.path })}>Ignore file</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'dismiss' })}>Dismiss</button></div>
  </>
}

function PathItem({ item, apply, busy }: ItemProps<'unsupported' | 'unreadable'>) {
  return <>
    <div className="root-path" title={item.details.path}>{item.details.path}</div>
    <p className="muted-text">{item.details.reason}</p>
    <div className="settings-actions"><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'ignore', path: item.details.path })}>Ignore</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'dismiss' })}>Dismiss</button></div>
  </>
}

function ShortcutItem({ item, apply, busy }: ItemProps<'shortcut'>) {
  return <>
    <p className="muted-text">This shortcut starts a program with extra arguments (or something other than a game .exe). It is only added once you confirm exactly what it runs.</p>
    <div className="root-path" title={item.details.shortcut}>{item.details.shortcut}</div>
    <dl className="review-shortcut"><dt>Runs</dt><dd className="root-path" title={item.details.target}>{item.details.target || '(nothing)'}</dd><dt>Arguments</dt><dd className="root-path" title={item.details.arguments}>{item.details.arguments || '(none)'}</dd></dl>
    <div className="settings-actions"><button type="button" className="primary-button" disabled={busy} onClick={() => apply({ action: 'confirm-shortcut' })}>Confirm and add</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'ignore', path: item.details.shortcut })}>Ignore</button><button type="button" className="secondary-button" disabled={busy} onClick={() => apply({ action: 'dismiss' })}>Dismiss</button></div>
  </>
}

const kindLabels: Record<Exclude<ReviewItem['kind'], HealthKind>, string> = { duplicate: 'Possible duplicate', 'ambiguous-exe': 'Pick the target', unsupported: 'Unsupported', unreadable: 'Unreadable folder', shortcut: 'Confirm shortcut', 'uncertain-rom': 'Uncertain ROM match' }
const itemTitle = (item: ReviewItem) => isHealthItem(item) ? healthTitle(item) : item.kind === 'duplicate' ? item.details.title + ' · ' + item.details.system : item.kind === 'ambiguous-exe' || item.kind === 'shortcut' || item.kind === 'uncertain-rom' ? item.details.title : item.details.path.split(/[\\/]/).filter(Boolean).pop() ?? item.details.path

export function ReviewSection({ say }: { say(text: string): void }) {
  const [items, setItems] = useState<ReviewItem[]>([])
  const [ignored, setIgnored] = useState<IgnoredPath[]>([])
  const [error, setError] = useState('')
  const [busyId, setBusyId] = useState<string | null>(null)
  const [loaded, setLoaded] = useState(false)
  const reload = useCallback(() => Promise.all([window.lobby.review.list().then(setItems), window.lobby.review.ignored().then(setIgnored)])
    .then(() => setError('')).catch(caught => setError(errorText(caught))).finally(() => setLoaded(true)), [])
  useEffect(() => {
    void reload()
    return window.lobby.review.onChanged(() => { void reload() })
  }, [reload])
  const apply = async (id: string, resolution: ReviewResolution) => {
    setBusyId(id); setError('')
    try { setItems(await window.lobby.review.resolve(id, resolution)); say('Decision saved'); void window.lobby.review.ignored().then(setIgnored) }
    catch (caught) { setError(errorText(caught)); void reload() }
    finally { setBusyId(null) }
  }
  const run = async (id: string, action: () => Promise<ReviewItem[]>, done: string) => {
    setBusyId(id); setError('')
    try { setItems(await action()); say(done) }
    catch (caught) { setError(errorText(caught)); void reload() }
    finally { setBusyId(null) }
  }
  const unignore = async (path: string) => {
    try { setIgnored(await window.lobby.review.unignore(path)); say('Restored. It will be included in the next scan.') }
    catch (caught) { setError(errorText(caught)) }
  }
  return <>
    <HealthCheckCard say={say} onError={setError} />
    <div className="settings-card">
      <h3>Needs review ({items.length})</h3>
      <p className="muted-text">Things a scan won’t decide on its own. Rescans refresh these instead of adding copies.</p>
      {error && <div className="form-error" role="alert">{error}</div>}
      {loaded && items.length === 0 && <div className="empty-inline">Nothing to review. Duplicates, ambiguous games and unreadable folders show up here after a scan.</div>}
      {items.map(item => <div className="review-item" key={item.id}>
        <div className="emu-name"><span className="status-pill">{isHealthItem(item) ? healthLabels[item.kind] : kindLabels[item.kind]}</span><b>{itemTitle(item)}</b></div>
        {isHealthItem(item) && <HealthItem item={item} busy={busyId === item.id} run={(action, done) => void run(item.id, action, done)} />}
        {item.kind === 'duplicate' && <DuplicateItem item={item} busy={busyId === item.id} apply={resolution => void apply(item.id, resolution)} />}
        {item.kind === 'ambiguous-exe' && <ExeItem item={item} busy={busyId === item.id} apply={resolution => void apply(item.id, resolution)} />}
        {(item.kind === 'unsupported' || item.kind === 'unreadable') && <PathItem item={item} busy={busyId === item.id} apply={resolution => void apply(item.id, resolution)} />}
        {item.kind === 'shortcut' && <ShortcutItem item={item} busy={busyId === item.id} apply={resolution => void apply(item.id, resolution)} />}
        {item.kind === 'uncertain-rom' && <UncertainRomItem item={item} busy={busyId === item.id} apply={resolution => void apply(item.id, resolution)} />}
      </div>)}
    </div>
    <div className="settings-card">
      <details className="collapsible"><summary>Ignored paths ({ignored.length})</summary>
        <p className="muted-text">Scans skip these files and folders. Restore one to include it again.</p>
        {ignored.length === 0 && <div className="empty-inline">Nothing ignored.</div>}
        {ignored.map(item => <div className="root-row" key={item.path}><div className="root-main"><div className="root-path" title={item.path}>{item.path}</div></div><button type="button" className="secondary-button" onClick={() => void unignore(item.path)}>Restore</button></div>)}
      </details>
    </div>
  </>
}
