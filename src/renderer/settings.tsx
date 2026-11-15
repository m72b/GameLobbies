import './lobby-api'
import { useEffect, useState } from 'react'
import { ArrowLeft, Blocks, ChevronRight, CircleHelp, Cloud, FolderSearch, Gamepad2, Keyboard, Library, ListChecks, Puzzle, Rocket, RotateCcw, Settings2, SlidersHorizontal, Sparkles, Star } from 'lucide-react'
import type { IgdbBulkProgress, IgdbCredentialStatus, Settings, SettingsSection, SteamGridDbStyle, StoreId, StoreStatus } from '../shared/types'
import { steamGridDbStyles } from '../shared/types'
import { defaultSettings } from '../shared/defaults'
import { applyTheme } from './theme'
import { StoreIcon } from './store-icons'
import { EmulatorsSection, LibraryFoldersSection, useLocalScan } from './library-settings'
import { ReviewSection } from './review-panel'
import { LaunchersSection } from './launcher-settings'
import { UpdatesSection } from './updates-section'
import { IgdbAttribution } from './igdb-details'
import { IgdbSetup, cleanPaste } from './igdb-setup'
import { createRoot } from 'react-dom/client'
import './styles.css'

const sections = [
  ['Libraries', 'Stores and launchers Game Lobby detects on this PC, and how often it scans.', Library],
  ['Library folders', 'Choose game and emulator folders and scan them.', FolderSearch],
  ['Emulators', 'Manage emulator profiles and the systems they run.', Gamepad2],
  ['Custom launchers', 'Your own launchers: name, icon, colour and how they start a game.', Rocket],
  ['Review', 'Decide on duplicates and games a scan could not place.', ListChecks],
  ['Appearance', 'Poster size and motion, cover art from SteamGridDB, and themes.', SlidersHorizontal],
  ['Startup and Behavior', 'Choose how the app opens and closes.', Settings2],
  ['Playtime and Stats', 'Playtime tracking, game details from IGDB, and the cover cache.', Star],
  ['Collections and Tags', 'Organize the library into collections.', Blocks],
  ['Backup and Sync', 'Export or restore your local library.', Cloud],
  ['Keyboard Shortcuts', 'Browse available keyboard controls.', Keyboard],
  ['Plugins / Add-ons', 'Install declarative community library plugins.', Puzzle],
  ['About and Updates', 'Version, project information and updates.', CircleHelp]
] as const

const sgdbStyleLabels: Record<SteamGridDbStyle, string> = { alternate: 'Alternate', blurred: 'Blurred', white_logo: 'White logo', material: 'Material', no_logo: 'No logo' }

type Section = typeof sections[number][0]

/** Sections other windows can open with `settings.open(id)` (`settings.html?section=<id>`). */
const sectionIds: Record<SettingsSection, Section> = { updates: 'About and Updates', 'cover-art': 'Appearance', metadata: 'Playtime and Stats' }
/** Cards a deep link scrolls to once its section is shown. */
const sectionAnchors: Partial<Record<SettingsSection, string>> = { 'cover-art': 'settings-cover-art', metadata: 'settings-metadata' }
const anchorFor = (id: string | null) => id !== null && Object.prototype.hasOwnProperty.call(sectionAnchors, id) ? sectionAnchors[id as SettingsSection] : undefined
const sectionFor = (id: string | null): Section | undefined => id !== null && Object.prototype.hasOwnProperty.call(sectionIds, id) ? sectionIds[id as SettingsSection] : undefined
const initialSection = (): Section => sectionFor(new URLSearchParams(window.location.search).get('section')) ?? 'Libraries'

function storeStatusText(store: StoreStatus) {
  if (!store.enabled) return 'Off · not scanned'
  if (store.error) return 'Could not read: ' + store.error
  if (store.found === undefined) return 'Not scanned yet'
  return store.found > 0 ? 'Detected · ' + store.found + ' game' + (store.found === 1 ? '' : 's') : 'Not detected on this PC'
}

export function SettingsApp() {
  const [section, setSection] = useState<Section>(initialSection)
  const [anchor, setAnchor] = useState<string | undefined>(() => anchorFor(new URLSearchParams(window.location.search).get('section')))
  const [settings, setSettings] = useState<Settings>(defaultSettings)
  const [plugins, setPlugins] = useState<any[]>([])
  const localScan = useLocalScan()
  const [themeUrl, setThemeUrl] = useState('')
  const [repoUrl, setRepoUrl] = useState('')
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const [stores, setStores] = useState<StoreStatus[]>([])
  const [sgdbKeySet, setSgdbKeySet] = useState(false)
  const [sgdbKey, setSgdbKey] = useState('')
  const [sgdbBusy, setSgdbBusy] = useState(false)
  const [sgdbFetching, setSgdbFetching] = useState(false)
  const [sgdbTest, setSgdbTest] = useState<{ ok: boolean; message: string } | null>(null)
  const [sgdbTesting, setSgdbTesting] = useState(false)
  const [igdbStatus, setIgdbStatus] = useState<IgdbCredentialStatus>({ clientIdSet: false, clientSecretSet: false })
  const [igdbClientId, setIgdbClientId] = useState('')
  const [igdbSecret, setIgdbSecret] = useState('')
  const [igdbBusy, setIgdbBusy] = useState(false)
  const [igdbTest, setIgdbTest] = useState<{ ok: boolean; message: string } | null>(null)
  const [igdbTesting, setIgdbTesting] = useState(false)
  const [igdbProgress, setIgdbProgress] = useState<IgdbBulkProgress | null>(null)

  useEffect(() => {
    const offSettings = window.lobby.settings.onChanged(setSettings)
    const offSection = window.lobby.settings.onSection(id => { const next = sectionFor(id); if (next) { setSection(next); setAnchor(anchorFor(id)) } })
    const offIgdb = window.lobby.igdb.onProgress(setIgdbProgress)
    void Promise.all([
      window.lobby.settings.get().then(setSettings),
      window.lobby.plugins.list().then(setPlugins),
      window.lobby.library.stores().then(setStores).catch(() => undefined),
      window.lobby.steamGridDb.status().then(result => setSgdbKeySet(result.keySet)).catch(() => undefined),
      window.lobby.igdb.status().then(setIgdbStatus).catch(() => undefined),
      window.lobby.igdb.progress().then(next => { if (next.total > 0 || next.running) setIgdbProgress(next) }).catch(() => undefined)
    ])
    return () => { offSettings(); offSection(); offIgdb() }
  }, [])
  useEffect(() => { applyTheme(settings.theme, settings) }, [settings])
  useEffect(() => {
    if (!anchor) return
    const frame = requestAnimationFrame(() => { document.getElementById(anchor)?.scrollIntoView({ block: 'start' }); setAnchor(undefined) })
    return () => cancelAnimationFrame(frame)
  }, [anchor, section])

  const say = (text: string) => {
    setMessage(text)
    window.setTimeout(() => setMessage(''), 3500)
  }
  const saveSgdbKey = async (key: string) => {
    setSgdbBusy(true)
    try {
      const result = await window.lobby.steamGridDb.saveKey(key)
      setSgdbKeySet(result.keySet)
      setSgdbKey('')
      setSgdbTest(null)
      say(result.keySet ? 'SteamGridDB key saved' : 'SteamGridDB key removed')
    } catch (error) { say(error instanceof Error ? error.message : 'Could not save the SteamGridDB key') }
    finally { setSgdbBusy(false) }
  }
  const testSgdbKey = async () => {
    setSgdbTesting(true)
    try {
      const result = await window.lobby.steamGridDb.testKey()
      setSgdbTest(result)
      say(result.message)
    } catch (error) {
      const text = error instanceof Error ? error.message : 'Could not test the SteamGridDB key'
      setSgdbTest({ ok: false, message: text })
      say(text)
    } finally { setSgdbTesting(false) }
  }
  const igdbConfigured = igdbStatus.clientIdSet && igdbStatus.clientSecretSet
  const saveIgdb = async () => {
    setIgdbBusy(true)
    try {
      const result = await window.lobby.igdb.saveCredentials(cleanPaste(igdbClientId), cleanPaste(igdbSecret))
      setIgdbStatus(result)
      setIgdbClientId(''); setIgdbSecret(''); setIgdbTest(null)
      say(result.clientIdSet && result.clientSecretSet ? 'IGDB credentials saved' : 'Saved. Enter both the Client ID and the Client Secret.')
    } catch (error) { say(error instanceof Error ? error.message : 'Could not save the IGDB credentials') }
    finally { setIgdbBusy(false) }
  }
  const clearIgdb = async () => {
    setIgdbBusy(true)
    try {
      setIgdbStatus(await window.lobby.igdb.clearCredentials())
      setIgdbTest(null)
      say('IGDB credentials removed')
    } catch (error) { say(error instanceof Error ? error.message : 'Could not remove the IGDB credentials') }
    finally { setIgdbBusy(false) }
  }
  const testIgdb = async () => {
    setIgdbTesting(true)
    try {
      const result = await window.lobby.igdb.test()
      setIgdbTest(result)
      say(result.message)
    } catch (error) {
      const text = error instanceof Error ? error.message : 'Could not test the IGDB connection'
      setIgdbTest({ ok: false, message: text })
      say(text)
    } finally { setIgdbTesting(false) }
  }
  const fetchAllIgdb = async () => {
    try { setIgdbProgress(await window.lobby.igdb.fetchAll()) }
    catch (error) { say(error instanceof Error ? error.message : 'Could not start the IGDB fetch') }
  }
  const toggleSgdbStyle = (style: SteamGridDbStyle) => {
    const current = settings.steamGridDbStyles
    void save({ steamGridDbStyles: current.includes(style) ? current.filter(item => item !== style) : [...current, style] })
  }
  const fetchSgdbCovers = async () => {
    setSgdbFetching(true)
    try {
      const result = await window.lobby.steamGridDb.fetchMissing()
      say('Added ' + result.updated + ' cover' + (result.updated === 1 ? '' : 's') + ' · ' + result.notFound + ' not found' + (result.errors.length ? ' · ' + result.errors[0] : ''))
    } catch (error) { say(error instanceof Error ? error.message : 'Cover fetch failed') }
    finally { setSgdbFetching(false) }
  }
  const save = async (patch: Partial<Settings>) => {
    try { setSettings(await window.lobby.settings.save(patch)) }
    catch (error) { say(error instanceof Error ? error.message : 'Settings could not be saved') }
  }
  const doTheme = async () => {
    setBusy(true)
    try {
      const next = await window.lobby.theme.load(themeUrl.trim())
      setSettings(next)
      say('Loaded remote theme: ' + next.theme.name)
    } catch (error) { say(error instanceof Error ? error.message : 'Theme could not be loaded') }
    finally { setBusy(false) }
  }
  const resetTheme = async () => {
    const next = await window.lobby.theme.reset()
    setSettings(next)
    setThemeUrl('')
    say('Restored built-in dark theme')
  }
  const exportBackup = async () => {
    const path = await window.lobby.backup.export()
    if (path) say('Backup saved to ' + path)
  }
  const importBackup = async () => say('Imported ' + await window.lobby.backup.import() + ' games')
  const installPlugin = async () => {
    const list = await window.lobby.plugins.install()
    setPlugins(list)
    say('Plugin manifest added')
  }
  const toggleStore = (id: StoreId, enabled: boolean) => {
    const current = settings.disabledStores ?? []
    const next = enabled ? current.filter(item => item !== id) : [...current.filter(item => item !== id), id]
    setStores(list => list.map(store => store.id === id ? { ...store, enabled } : store))
    void save({ disabledStores: next })
  }
  const scanLibraries = async () => {
    try {
      const result = await window.lobby.library.scan()
      say(result.errors?.length ? 'Imported ' + (result.imported || 0) + ' records. ' + result.errors[0] : 'Scan complete · ' + result.total + ' games in library')
    } catch (error) { say(error instanceof Error ? error.message : 'Library scan failed') }
    finally { window.lobby.library.stores().then(setStores).catch(() => undefined) }
  }
  const resetPlaytime = async () => {
    await window.lobby.library.resetPlaytime()
    say('Playtime and last-played statistics reset')
  }

  return <div className="settings-shell">
    <header className="titlebar settings-titlebar">
      <button className="back-button" onClick={() => window.close()}><ArrowLeft size={15} /></button>
      <div className="brand">Game Lobby <span>/ Settings</span></div><div className="drag-space" data-tauri-drag-region />
      <button className="title-action" onClick={() => window.lobby.window.minimize()}>—</button>
      <button className="title-action" onClick={() => window.lobby.window.toggleMaximize()}>□</button>
      <button className="title-action close" onClick={() => window.lobby.window.close()}>×</button>
    </header>
    <div className="settings-layout">
      <nav className="settings-nav">
        <div className="settings-nav-label">PREFERENCES</div>
        {sections.map(([name, , Icon]) => <button key={name} className={section === name ? 'selected' : ''} onClick={() => setSection(name)}><Icon size={16} /><span>{name}</span><ChevronRight size={13} /></button>)}
      </nav>
      <main className="settings-content">
        <div className="eyebrow">SETTINGS</div><h1>{section}</h1>
        <p className="settings-description">{sections.find(item => item[0] === section)?.[1]}</p>

        {section === 'Libraries' && <>
          <div className="settings-card">
            <h3>Detected stores and launchers</h3>
            <p className="muted-text">Game Lobby finds installed games on its own by reading each launcher's local files on this PC. No accounts or keys are needed. Turn a store off to stop scanning it; games already in the library stay until you remove them.</p>
            {stores.map(store => <div className="toggle-row store-row" key={store.id}>
              <span className="store-badge small"><StoreIcon store={store.name} /></span>
              <div><b>{store.name}</b><small className={store.error ? 'store-error' : ''}>{storeStatusText(store)}</small></div>
              <input type="checkbox" aria-label={'Scan ' + store.name} checked={store.enabled} onChange={event => toggleStore(store.id, event.target.checked)} />
            </div>)}
            <div className="settings-actions"><button className="primary-button" onClick={() => void scanLibraries()}><Library size={15} /> Scan libraries</button></div>
          </div>
          <div className="settings-card"><div className="toggle-row"><div><b>Show only installed games</b><small>Hide games that are not installed</small></div><input type="checkbox" checked={settings.showOnlyInstalled} onChange={event => void save({ showOnlyInstalled: event.target.checked })} /></div><div className="toggle-row"><div><b>Minimize when a game starts</b><small>Game Lobby moves to the taskbar after you press Play</small></div><input type="checkbox" checked={settings.minimizeOnLaunch} onChange={event => void save({ minimizeOnLaunch: event.target.checked })} /></div></div>
          <label className="setting-row">Scan interval<select value={settings.scanIntervalHours} onChange={event => void save({ scanIntervalHours: +event.target.value })}><option value={0}>Manual only</option><option value={6}>Every 6 hours</option><option value={12}>Every 12 hours</option><option value={24}>Daily</option></select></label>
        </>}

        {section === 'Library folders' && <LibraryFoldersSection scan={localScan} onOpenReview={() => setSection('Review')} say={say} />}
        {section === 'Emulators' && <EmulatorsSection scan={localScan} say={say} />}
        {section === 'Custom launchers' && <LaunchersSection say={say} />}
        {section === 'Review' && <ReviewSection say={say} />}

        {section === 'Appearance' && <>
          <div className="settings-card">
            <h3>Library appearance</h3>
            <label className="setting-row">Poster width <span>{settings.posterWidth}px</span><input type="range" min="120" max="250" value={settings.posterWidth} onChange={event => void save({ posterWidth: +event.target.value })} /></label>
            <div className="toggle-row"><div><b>Store badges</b><small>Show a store mark on each poster</small></div><input type="checkbox" checked={settings.showBadges} onChange={event => void save({ showBadges: event.target.checked })} /></div>
            <div className="toggle-row"><div><b>Animations</b><small>Enable poster hover motion</small></div><input type="checkbox" checked={settings.animations} onChange={event => void save({ animations: event.target.checked })} /></div>
            <div className="feature-row"><span>Grid density</span><span className="coming-soon">COMING SOON</span></div>
          </div>
          <div className="settings-card" id="settings-cover-art">
          <div className="sgdb-card plain">
            <h3>Cover art</h3>
            <p>Community cover art from SteamGridDB. It needs a free API key: sign in at steamgriddb.com, then open Profile → Preferences → API. <a className="external-link" href="https://www.steamgriddb.com/profile/preferences/api">Get an API key</a></p>
            <form className="sgdb-key-row" onSubmit={event => { event.preventDefault(); if (sgdbKey.trim()) void saveSgdbKey(sgdbKey.trim()) }}>
              <input type="password" aria-label="SteamGridDB API key" value={sgdbKey} onChange={event => setSgdbKey(event.target.value)} placeholder={sgdbKeySet ? 'Enter a new key to replace' : 'Paste your API key'} autoComplete="new-password" spellCheck={false} />
              <button className="secondary-button" disabled={sgdbBusy || !sgdbKey.trim()}>Save</button>
              {sgdbKeySet && <button type="button" className="secondary-button" disabled={sgdbBusy} onClick={() => void saveSgdbKey('')}>Remove key</button>}
            </form>
            <div className="sgdb-status-row"><span className="sgdb-status">{sgdbKeySet ? 'Key saved' : 'No key set'}</span><button className="secondary-button" disabled={!sgdbKeySet || sgdbTesting} onClick={() => void testSgdbKey()}>{sgdbTesting ? 'Testing…' : 'Test key'}</button>{sgdbTest && <span className={'sgdb-test ' + (sgdbTest.ok ? 'ok' : 'fail')}>{sgdbTest.ok ? '✓' : '✗'} {sgdbTest.message}</span>}</div>
            <div className="toggle-row"><div><b>Fetch missing covers after each scan</b><small>Fill games that have no cover from SteamGridDB</small></div><input type="checkbox" checked={settings.steamGridDbAutoFetch} onChange={event => void save({ steamGridDbAutoFetch: event.target.checked })} /></div>
            <button className="secondary-button" disabled={!sgdbKeySet || sgdbFetching} onClick={() => void fetchSgdbCovers()}>{sgdbFetching ? 'Fetching covers…' : 'Fetch missing covers now'}</button>
          </div>
          </div>
          <div className="settings-card">
            <h3>Cover art preferences</h3>
            <p className="muted-text">Applied when searching for covers and when fetching missing ones.</p>
            <div className="chip-group" role="group" aria-label="Cover styles">{steamGridDbStyles.map(style => <button key={style} type="button" className={'chip ' + (settings.steamGridDbStyles.includes(style) ? 'active' : '')} aria-pressed={settings.steamGridDbStyles.includes(style)} onClick={() => toggleSgdbStyle(style)}>{sgdbStyleLabels[style]}</button>)}</div>
            <small className="chip-hint">None selected = any style</small>
            <div className="toggle-row"><div><b>Include animated covers</b></div><input type="checkbox" checked={settings.steamGridDbAnimated} onChange={event => void save({ steamGridDbAnimated: event.target.checked })} /></div>
            <div className="toggle-row"><div><b>Include adult (NSFW) covers</b></div><input type="checkbox" checked={settings.steamGridDbNsfw} onChange={event => void save({ steamGridDbNsfw: event.target.checked })} /></div>
            <div className="toggle-row"><div><b>Include humor covers</b></div><input type="checkbox" checked={settings.steamGridDbHumor} onChange={event => void save({ steamGridDbHumor: event.target.checked })} /></div>
          </div>
          <div className="settings-card">
            <h3>Themes</h3><p className="muted-text settings-subhead">MyUI remote theme</p><p>Load token-only JSON or CSS from a trusted HTTPS URL. Theme content is validated and applied live in both windows. No remote scripts are executed.</p>
            <label className="field-label">Theme URL<input value={themeUrl || settings.themeUrl || ''} onChange={event => setThemeUrl(event.target.value)} placeholder="https://example.com/my-theme.json" /></label>
            <div className="settings-actions"><button className="primary-button" disabled={busy || !themeUrl.trim()} onClick={() => void doTheme()}><Sparkles size={15} />{busy ? 'Loading…' : 'Load theme'}</button><button className="secondary-button" onClick={() => void resetTheme()}><RotateCcw size={15} /> Reset to default</button></div>
            {settings.themeName && settings.themeUrl && <div className="remote-tag">REMOTE THEME · {settings.themeName}</div>}
          </div>
          <div className="settings-card"><h3>Theme repository</h3><p>Theme repository browsing is not available yet.</p><label className="field-label">Repository index URL<input value={repoUrl} onChange={event => setRepoUrl(event.target.value)} placeholder="Coming soon" disabled /></label><span className="coming-soon">COMING SOON</span></div>
        </>}

        {section === 'Startup and Behavior' && <div className="settings-card">
          {['Launch on boot', 'Minimize to tray', 'Close to tray'].map(label => <div className="feature-row" key={label}><span>{label}</span><span className="coming-soon">COMING SOON</span></div>)}
        </div>}

        {section === 'Playtime and Stats' && <>
          <div className="settings-card">
            <h3>Playtime</h3>
            <div className="toggle-row"><div><b>Track playtime</b><small>Store session time in the local library</small></div><input type="checkbox" checked={settings.tracking} onChange={event => void save({ tracking: event.target.checked })} /></div>
            <div className="toggle-row"><div><b>Detect games launched outside Game Lobby</b><small>Tracks games started from Steam, Epic or shortcuts while Game Lobby is open</small></div><input type="checkbox" checked={settings.detectExternalLaunches} disabled={!settings.tracking} onChange={event => void save({ detectExternalLaunches: event.target.checked })} /></div>
            <button className="secondary-button" onClick={() => void resetPlaytime()}>Reset playtime stats</button>
          </div>
          <div className="settings-card" id="settings-metadata">
          <div className="sgdb-card plain">
            <h3>Metadata provider</h3>
            <IgdbSetup status={igdbStatus} clientId={igdbClientId} secret={igdbSecret} busy={igdbBusy} testing={igdbTesting} test={igdbTest} onClientId={setIgdbClientId} onSecret={setIgdbSecret} onSave={() => void saveIgdb()} onClear={() => void clearIgdb()} onTest={() => void testIgdb()} />
            <h4 className="settings-subtitle">Whole library</h4>
            <p className="muted-text">Game pages load details from IGDB when you open them and refresh them after 30 days. You can also fetch details for every game now. It runs in the background at about four requests per second.</p>
            <div className="settings-actions">
              <button className="secondary-button" disabled={!igdbConfigured || !!igdbProgress?.running} onClick={() => void fetchAllIgdb()}>{igdbProgress?.running ? 'Fetching…' : 'Fetch metadata for all games'}</button>
              {igdbProgress?.running && <button className="secondary-button" onClick={() => void window.lobby.igdb.cancelFetchAll()}>Cancel</button>}
            </div>
            {igdbProgress && <div className="sgdb-status igdb-progress" role="status">
              {igdbProgress.running ? 'Fetching ' : 'Done: '}{igdbProgress.done} of {igdbProgress.total} · {igdbProgress.updated} found · {igdbProgress.notFound} not found{igdbProgress.failed ? ' · ' + igdbProgress.failed + ' failed' : ''}{igdbProgress.message ? ' · ' + igdbProgress.message : ''}
            </div>}
            <div className="igdb-links"><IgdbAttribution /></div>
          </div>
          </div>
          <div className="settings-card">
            <h3>Covers and cache</h3>
            <p className="muted-text">Steam cover art is used automatically when an app ID is available. Cover art from SteamGridDB is set up under Appearance.</p>
            <div className="feature-row"><span>Default artwork style</span><span>Poster (2:3)</span></div>
            <div className="feature-row"><span>Cover cache</span><span className="status-pill">Stored locally</span></div>
            <div className="feature-row"><span>Custom cover images</span><span className="status-pill">User data folder</span></div>
          </div>
        </>}

        {section === 'Collections and Tags' && <div className="settings-card">
          <h3>Collections</h3><p>Collection creation and tags are not available yet.</p>
          <button className="secondary-button coming-soon-control" disabled>Manage collections · Coming soon</button>
        </div>}

        {section === 'Backup and Sync' && <div className="settings-card">
          <h3>Local backup</h3><p>Export titles, routes, settings and emulator profiles to JSON. Image files are not included: covers and your screenshots stay in Game Lobby's local data folder, and screenshots must be added again on another PC.</p>
          <div className="settings-actions"><button className="primary-button" onClick={() => void exportBackup()}>Export library</button><button className="secondary-button" onClick={() => void importBackup()}>Import backup</button></div>
          <div className="feature-row"><span>Automatic backup</span><span className="coming-soon">COMING SOON</span></div>
        </div>}

        {section === 'Keyboard Shortcuts' && <div className="settings-card shortcuts">
          {[
            ['Ctrl + K', 'Focus search'], ['Enter', 'Launch focused game'], ['Escape', 'Close detail or modal'],
            ['Arrow keys', 'Move grid focus and scroll'], ['Right click', 'Open game menu']
          ].map(([key, action]) => <div className="feature-row" key={key}><kbd>{key}</kbd><span>{action}</span></div>)}
        </div>}

        {section === 'Plugins / Add-ons' && <div className="settings-card">
          <div className="card-heading"><div><h3>Community plugins</h3><p>Install declarative JSON manifests. Plugin JavaScript is not downloaded or executed.</p></div><button className="secondary-button" onClick={() => void installPlugin()}><Puzzle size={14} /> Add manifest</button></div>
          {plugins.map(plugin => <div className="feature-row" key={plugin.id}>{plugin.name}<span className="status-pill">v{plugin.version}</span></div>)}
          <a className="external-link" href="https://playnite.link/addons.html">Browse the Playnite add-on catalog</a>
        </div>}

        {section === 'About and Updates' && <UpdatesSection autoCheck={settings.autoCheckUpdates} onAutoCheckChange={enabled => void save({ autoCheckUpdates: enabled })} />}
        {message && <div className="settings-toast">{message}</div>}
      </main>
    </div>
  </div>
}

createRoot(document.getElementById('root')!).render(<SettingsApp />)
