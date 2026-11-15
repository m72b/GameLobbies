import { useCallback, useEffect, useRef, useState } from 'react'
import type { CatalogSystem, EmulatorCatalog, EmulatorProfile, IgnoredPath, LibraryRoot, LibraryRootKind, LocalScanProgress, LocalScanSummary } from '../shared/types'
import { libraryRoots, scanSummaryLines } from '../shared/launchers'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

export const formatDuration = (ms: number) => ms < 1000 ? ms + ' ms' : (ms / 1000).toFixed(1) + ' s'
const formatWhen = (time?: number) => time ? new Date(time).toLocaleString() : 'Never scanned'
const truncatePath = (path: string, max = 70) => path.length <= max ? path : path.slice(0, 24) + '…' + path.slice(path.length - (max - 25))
export const alreadyRunning = (text: string) => /scan is already running/i.test(text) ? 'A scan is already running. Wait for it to finish, then try again.' : text

/** Scan state that outlives the settings sections, so progress isn't lost when switching pages. */
export function useLocalScan() {
  const [progress, setProgress] = useState<LocalScanProgress | null>(null)
  const [summary, setSummary] = useState<LocalScanSummary | null>(null)
  const [running, setRunning] = useState(false)
  useEffect(() => {
    const offProgress = window.lobby.roots.onProgress(next => { setRunning(next.phase !== 'done'); setProgress(next) })
    const offFinished = window.lobby.roots.onFinished(next => { setSummary(next); setProgress(null); setRunning(false) })
    return () => { offProgress(); offFinished() }
  }, [])
  const dismiss = useCallback(() => setSummary(null), [])
  return { progress, summary, running, dismiss, setSummary }
}
export type LocalScan = ReturnType<typeof useLocalScan>

/** A quick scan that stopped at its cap: explain it and offer a full scan. Not an error. */
function FullScanNotice({ notice, busy, onFullScan, onNotNow }: { notice: string; busy: boolean; onFullScan(): void; onNotNow(): void }) {
  return <div className="inline-warning inline-notice" role="status">
    <p>{notice}</p>
    <div className="settings-actions tight">
      <button type="button" className="primary-button" disabled={busy} onClick={onFullScan}>Scan everything (may take a while)</button>
      <button type="button" className="secondary-button" onClick={onNotNow}>Not now</button>
    </div>
  </div>
}

const phaseLabels: Record<LocalScanProgress['phase'], string> = { emulators: 'Looking for emulators', games: 'Scanning game folders', library: 'Saving library and checking duplicates', done: 'Finishing' }

/** Scan progress (`library-scan:progress`), shared by Library folders and Custom launchers. */
export function ScanProgress({ progress }: { progress: LocalScanProgress | null }) {
  return <div className="scan-progress" role="status" aria-live="polite">
    <div className="scan-bar"><span className={progress?.total ? '' : 'indeterminate'} style={progress?.total ? { width: Math.min(100, progress.processed / progress.total * 100) + '%' } : undefined} /></div>
    <div className="scan-line"><b>{progress ? phaseLabels[progress.phase] : 'Starting scan…'}</b>{progress && progress.phase !== 'library' && progress.phase !== 'done' && <span>{progress.processed}{progress.total ? ' / ' + progress.total : ''} entries</span>}</div>
    {progress?.root && progress.phase !== 'library' && <div className="scan-path" title={progress.root}>Root: {truncatePath(progress.root)}</div>}
    {progress?.current && <div className="scan-path" title={progress.current}>{truncatePath(progress.current)}</div>}
  </div>
}

export function ScanSummary({ summary, busy, notNow, onClose, onOpenReview, onFullScan, onNotNow }: { summary: LocalScanSummary; busy: boolean; notNow: Set<string>; onClose(): void; onOpenReview?(): void; onFullScan(id: string): void; onNotNow(id: string): void }) {
  const problemRoots = summary.roots.filter(root => !root.ok || root.unreadable > 0 || !!root.warning)
  const noticeRoots = summary.roots.filter(root => root.ok && root.notice && !notNow.has(root.id))
  return <div className="scan-summary" role="status">
    <div className="card-heading"><h3>Last scan · {formatDuration(summary.durationMs)}</h3><button type="button" className="secondary-button" onClick={onClose}>Dismiss</button></div>
    <div className="scan-counts">
      {scanSummaryLines(summary).map(([label, value]) => <div key={label}><span>{label}</span><b>{value}</b>{label === 'Needs review' && summary.review > 0 && onOpenReview && <button type="button" className="link-button" onClick={onOpenReview}>Open review</button>}</div>)}
      <div><span>Details</span><b>Emulators: {summary.emulators.updated} updated · {summary.emulators.unavailable} unavailable. Games: {summary.games.updated} updated · {summary.games.removedOrUninstalled} removed or uninstalled · {summary.games.failed} unreadable folders{summary.games.removalsHeld > 0 ? ' · ' + summary.games.removalsHeld + ' removals held back' : ''}</b></div>
    </div>
    {problemRoots.map(root => <div className="inline-warning" key={root.id}>{root.path}: {!root.ok ? (root.error || 'could not be read') : [root.unreadable > 0 ? root.unreadable + ' unreadable folder' + (root.unreadable === 1 ? '' : 's') : '', root.warning ?? ''].filter(Boolean).join(' · ')}</div>)}
    {noticeRoots.map(root => <FullScanNotice key={'notice-' + root.id} notice={root.notice ?? ''} busy={busy} onFullScan={() => onFullScan(root.id)} onNotNow={() => onNotNow(root.id)} />)}
    {summary.unsupportedSamples.length > 0 && <details className="collapsible"><summary>Skipped or unsupported ({summary.unsupportedSamples.length}{summary.games.skipped > summary.unsupportedSamples.length ? ' shown' : ''})</summary>
      <ul className="path-list">{summary.unsupportedSamples.map((item, index) => <li key={index} title={item}>{item}</li>)}</ul></details>}
  </div>
}

function RootList({ title, kind, hint, roots, scanBusy, activeId, notNow, onChange, onRescan, onFullScan, onNotNow, onError }: { title: string; kind: LibraryRootKind; hint: string; roots: LibraryRoot[]; scanBusy: boolean; activeId: string | null; notNow: Set<string>; onChange(next: LibraryRoot[]): void; onRescan(id: string): void; onFullScan(id: string): void; onNotNow(id: string): void; onError(text: string): void }) {
  const [path, setPath] = useState('')
  const [error, setError] = useState('')
  const [adding, setAdding] = useState(false)
  const [confirming, setConfirming] = useState<string | null>(null)
  const mine = libraryRoots(roots).filter(root => root.kind === kind)
  const label = kind === 'emulators' ? 'Emulator folder path' : 'Game folder path'
  const browse = async () => {
    try { const chosen = await window.lobby.roots.browse(kind); if (chosen) { setPath(chosen); setError('') } }
    catch (caught) { setError(errorText(caught, 'Could not open the folder picker')) }
  }
  const add = async () => {
    if (!path.trim()) return
    setAdding(true); setError('')
    try { onChange(await window.lobby.roots.add(kind, path.trim())); setPath('') }
    catch (caught) { setError(errorText(caught, 'Could not add this folder')) }
    finally { setAdding(false) }
  }
  const run = async (action: () => Promise<LibraryRoot[]>) => {
    try { onChange(await action()) } catch (caught) { onError(errorText(caught, 'Could not update the folder')) }
  }
  return <div className="settings-card">
    <h3>{title}</h3>
    <p className="muted-text">{hint}</p>
    <form className="root-add" onSubmit={event => { event.preventDefault(); void add() }}>
      <input aria-label={label} value={path} readOnly onClick={() => void browse()} placeholder="Choose a folder with Browse…" title="Folders are chosen with Browse…, so only folders you picked are scanned" spellCheck={false} aria-invalid={!!error} />
      <button type="button" className="secondary-button" onClick={() => void browse()}>Browse…</button>
      <button type="submit" className="primary-button" disabled={adding || !path.trim()}>Add</button>
    </form>
    {error && <div className="form-error" role="alert">{error}</div>}
    {mine.length === 0 && <div className="empty-inline">No folders added yet.</div>}
    {mine.map(root => {
      const result = root.lastResult
      const counts = result?.games ? result.games.added + ' added · ' + result.games.updated + ' updated · ' + result.games.removedOrUninstalled + ' removed' : result?.emulators ? result.emulators.added + ' added · ' + result.emulators.updated + ' updated · ' + result.emulators.unavailable + ' unavailable' : ''
      return <div className={'root-row' + (root.enabled ? '' : ' disabled')} key={root.id}>
        <div className="root-main">
          <div className="root-path" title={root.path}>{root.path}</div>
          <div className="root-meta">
            <span className={'status-pill ' + (root.online ? 'ok' : 'warn')}>{root.online ? 'Online' : 'Offline'}</span>
            {!root.enabled && <span className="status-pill">Disabled</span>}
            <span>{formatWhen(root.lastScanAt)}</span>
            {result && (result.ok
              ? <span>{counts}{result.unreadable > 0 ? ' · ' + result.unreadable + ' unreadable folder' + (result.unreadable === 1 ? '' : 's') : ''}{result.full ? ' · full scan' : ''}</span>
              : <span className="text-warn">Failed: {result.error || 'could not be read'}</span>)}
          </div>
          {result?.ok && result.warning && <div className="inline-warning">{result.warning}</div>}
          {result?.ok && result.notice && !notNow.has(root.id) && root.enabled && <FullScanNotice notice={result.notice} busy={scanBusy} onFullScan={() => onFullScan(root.id)} onNotNow={() => onNotNow(root.id)} />}
        </div>
        <label className="root-toggle"><input type="checkbox" checked={root.enabled} onChange={event => void run(() => window.lobby.roots.setEnabled(root.id, event.target.checked))} aria-label={'Enable ' + root.path} /> Enabled</label>
        <button type="button" className="secondary-button" disabled={!root.enabled || scanBusy} onClick={() => onRescan(root.id)}>{activeId === root.id ? 'Scanning…' : 'Rescan'}</button>
        {confirming === root.id
          ? <span className="confirm-group"><span>Remove this folder?</span><button type="button" className="secondary-button danger" onClick={() => { setConfirming(null); void run(() => window.lobby.roots.remove(root.id)) }}>Remove</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
          : <button type="button" className="secondary-button danger" onClick={() => setConfirming(root.id)}>Remove</button>}
      </div>
    })}
  </div>
}

export function LibraryFoldersSection({ scan, onOpenReview, say }: { scan: LocalScan; onOpenReview(): void; say(text: string): void }) {
  const [roots, setRoots] = useState<LibraryRoot[]>([])
  const [loadError, setLoadError] = useState('')
  const [scanError, setScanError] = useState('')
  const [activeId, setActiveId] = useState<string | null>(null)
  // Roots whose "scan everything" offer was answered with "Not now" (until their next scan).
  const [notNow, setNotNow] = useState<Set<string>>(() => new Set())
  const hideNotice = (id: string) => setNotNow(current => new Set(current).add(id))
  const reload = useCallback(() => window.lobby.roots.list().then(next => { setRoots(next); setLoadError('') }).catch(caught => setLoadError(errorText(caught, 'Could not load library folders'))), [])
  useEffect(() => { void reload() }, [reload])
  const wasRunning = useRef(false)
  useEffect(() => { if (wasRunning.current && !scan.running) void reload(); wasRunning.current = scan.running }, [scan.running, reload])
  const rescan = async (id?: string, full = false) => {
    setScanError(''); setActiveId(id ?? 'all')
    setNotNow(current => { const next = new Set(current); if (id) next.delete(id); else next.clear(); return next })
    try { scan.setSummary(await window.lobby.roots.rescan(id, { full })); await reload() }
    catch (caught) { setScanError(alreadyRunning(errorText(caught, 'Scan failed'))) }
    finally { setActiveId(null) }
  }
  const scanning = activeId !== null || scan.running
  const { progress } = scan
  return <>
    <div className="settings-card">
      <div className="card-heading"><div><h3>Library folders</h3><p>Game folders contain one subfolder per system, named like “PS3”, “Sony Playstation 3”, “Switch” or “PC Game”, at any depth (or are one themselves). ROMs go inside those. Under “PC”, each game has its own subfolder. Emulator folders are searched for known emulators.</p></div>
        <button type="button" className="primary-button" disabled={scanning || roots.length === 0} onClick={() => void rescan()} title="Also scans the games folders of your custom launchers">{activeId === 'all' ? 'Scanning…' : 'Scan all'}</button></div>
      {loadError && <div className="form-error" role="alert">{loadError}</div>}
      {scanError && <div className="form-error" role="alert">{scanError}</div>}
      {scanning && <ScanProgress progress={progress} />}
      {scan.summary && !scanning && <ScanSummary summary={scan.summary} busy={scanning} notNow={notNow} onClose={scan.dismiss} onOpenReview={onOpenReview} onFullScan={id => void rescan(id, true)} onNotNow={hideNotice} />}
    </div>
    <RootList title="Game folders" kind="games" hint="Folders with your ROMs and PC games, organised by system. You can also add a single system folder, such as “Sony Playstation 2” or “PC Game”." roots={roots} scanBusy={scanning} activeId={activeId} notNow={notNow} onChange={setRoots} onRescan={id => void rescan(id)} onFullScan={id => void rescan(id, true)} onNotNow={hideNotice} onError={say} />
    <RootList title="Emulator folders" kind="emulators" hint="Folders searched (up to 5 levels deep) for emulator executables such as RetroArch, Dolphin or PCSX2. An emulator’s own data folders are skipped." roots={roots} scanBusy={scanning} activeId={activeId} notNow={notNow} onChange={setRoots} onRescan={id => void rescan(id)} onFullScan={id => void rescan(id, true)} onNotNow={hideNotice} onError={say} />
  </>
}

const blankProfile = (): EmulatorProfile => ({ id: crypto.randomUUID(), name: '', executable: '', args: '{rom}', romFolders: [], extensions: [], systems: [], enabled: true, source: 'manual' })

function EmulatorEditor({ initial, isNew, systems, onSaved, onCancel }: { initial: EmulatorProfile; isNew: boolean; systems: CatalogSystem[]; onSaved(list: EmulatorProfile[]): void; onCancel(): void }) {
  const [profile, setProfile] = useState(initial)
  const [extensions, setExtensions] = useState(initial.extensions.join(', '))
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const selected = profile.systems ?? []
  const toggleSystem = (id: string) => setProfile({ ...profile, systems: selected.includes(id) ? selected.filter(item => item !== id) : [...selected, id] })
  const browse = async () => {
    try { const chosen = await window.lobby.emulators.browseExecutable(); if (chosen) setProfile({ ...profile, executable: chosen }) }
    catch (caught) { setError(errorText(caught, 'Could not open the file picker')) }
  }
  const submit = async () => {
    setBusy(true); setError('')
    const list = extensions.split(/[\s,;]+/).map(item => item.trim().toLowerCase()).filter(Boolean).map(item => item.startsWith('.') ? item : '.' + item)
    try { onSaved(await window.lobby.emulators.save({ ...profile, name: profile.name.trim(), executable: profile.executable.trim(), extensions: list })) }
    catch (caught) { setError(errorText(caught, 'Emulator could not be saved')) }
    finally { setBusy(false) }
  }
  return <form className="emulator-editor" onSubmit={event => { event.preventDefault(); void submit() }}>
    <h4>{isNew ? 'Add emulator' : 'Edit ' + initial.name}</h4>
    <label className="field-label">Name<input value={profile.name} onChange={event => setProfile({ ...profile, name: event.target.value })} required /></label>
    <label className="field-label">Executable<span className="inline-field"><input value={profile.executable} readOnly onClick={() => void browse()} placeholder="Choose the emulator’s .exe with Browse…" title="The executable is chosen with Browse…, so only programs you picked are run" spellCheck={false} required /><button type="button" className="secondary-button" onClick={() => void browse()}>Browse…</button></span></label>
    <label className="field-label">Arguments<input value={profile.args} onChange={event => setProfile({ ...profile, args: event.target.value })} placeholder="{rom}" spellCheck={false} />
      <small className="chip-hint">Placeholders: <code>{'{rom}'}</code> ROM path, <code>{'{romdir}'}</code> its folder, <code>{'{romname}'}</code> file name without extension. Use quotes to group text with spaces, e.g. <code>{'-batch "{rom}"'}</code>; a path with spaces stays a single argument. No shell is used. Without <code>{'{rom}'}</code> the ROM path is appended last. Files named in the arguments must be inside the emulator’s folder, e.g. <code>{'-L cores\\snes9x_libretro.dll {rom}'}</code>.</small></label>
    <div className="field-label">Systems
      <div className="chip-group" role="group" aria-label="Systems this emulator runs">{systems.map(system => <button key={system.id} type="button" className={'chip ' + (selected.includes(system.id) ? 'active' : '')} aria-pressed={selected.includes(system.id)} onClick={() => toggleSystem(system.id)}>{system.name}</button>)}</div>
      {systems.length === 0 && <small className="chip-hint">System list unavailable.</small>}</div>
    <label className="field-label">Extensions (optional)<input value={extensions} onChange={event => setExtensions(event.target.value)} placeholder=".iso, .cue" spellCheck={false} /></label>
    {error && <div className="form-error" role="alert">{error}</div>}
    <div className="settings-actions"><button type="submit" className="primary-button" disabled={busy}>{busy ? 'Saving…' : 'Save'}</button><button type="button" className="secondary-button" onClick={onCancel}>Cancel</button></div>
  </form>
}

export function EmulatorsSection({ scan, say }: { scan: LocalScan; say(text: string): void }) {
  const [emulators, setEmulators] = useState<EmulatorProfile[]>([])
  const [catalog, setCatalog] = useState<EmulatorCatalog | null>(null)
  const [ignored, setIgnored] = useState<IgnoredPath[]>([])
  const [editing, setEditing] = useState<{ profile: EmulatorProfile; isNew: boolean } | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)
  const [detecting, setDetecting] = useState(false)
  const [error, setError] = useState('')
  const reload = useCallback(() => Promise.all([
    window.lobby.emulators.list().then(setEmulators),
    window.lobby.emulators.ignored().then(setIgnored)
  ]).catch(caught => setError(errorText(caught, 'Could not load emulators'))), [])
  useEffect(() => { void reload(); window.lobby.emulators.catalog().then(setCatalog).catch(() => undefined) }, [reload])
  const wasRunning = useRef(false)
  useEffect(() => { if (wasRunning.current && !scan.running) void reload(); wasRunning.current = scan.running }, [scan.running, reload])
  const systemName = (id: string) => catalog?.systems.find(system => system.id === id)?.name ?? id
  const act = async (action: () => Promise<unknown>, fallback: string) => {
    setError('')
    try { await action(); await reload() } catch (caught) { setError(errorText(caught, fallback)) }
  }
  const detect = async () => {
    setDetecting(true); setError('')
    try {
      const result = await window.lobby.emulators.scan()
      scan.setSummary(result)
      say('Emulators: ' + result.emulators.added + ' added · ' + result.emulators.updated + ' updated · ' + result.emulators.unavailable + ' unavailable')
      await reload()
    } catch (caught) { setError(alreadyRunning(errorText(caught, 'Emulator scan failed'))) }
    finally { setDetecting(false) }
  }
  return <>
    <div className="settings-card">
      <div className="card-heading"><div><h3>Emulator profiles</h3><p>Emulators are found in your emulator folders (see Library folders) or added by hand. Each one lists the systems it runs; a game uses the best matching enabled emulator unless you pick another on its page.</p></div>
        <div className="settings-actions tight"><button type="button" className="secondary-button" disabled={detecting || scan.running} onClick={() => void detect()}>{detecting ? 'Detecting…' : 'Detect emulators'}</button><button type="button" className="primary-button" onClick={() => setEditing({ profile: blankProfile(), isNew: true })}>Add emulator manually</button></div></div>
      {error && <div className="form-error" role="alert">{error}</div>}
      {editing?.isNew && <EmulatorEditor initial={editing.profile} isNew systems={catalog?.systems ?? []} onSaved={list => { setEmulators(list); setEditing(null); say('Emulator saved') }} onCancel={() => setEditing(null)} />}
      {emulators.length === 0 && <div className="empty-inline">No emulators yet. Add an emulator folder under Library folders and choose Detect emulators, or add one manually.</div>}
      {emulators.map(profile => {
        const isEditing = !!editing && !editing.isNew && editing.profile.id === profile.id
        return <div className="emu-item" key={profile.id}>
          <div className={'emu-row' + (profile.enabled === false ? ' disabled' : '')}>
            <div className="root-main">
              <div className="emu-name"><b>{profile.name}</b>
                {profile.detectedAs && <span className="status-pill">{catalog?.emulators.find(item => item.id === profile.detectedAs)?.name ?? profile.detectedAs}</span>}
                <span className="status-pill">{profile.source === 'discovered' ? 'Discovered' : 'Manual'}</span>
                {profile.available === false && <span className="status-pill warn">Not found</span>}
                {profile.needsConfiguration && <span className="status-pill warn">Needs setup</span>}
              </div>
              <div className="root-path" title={profile.executable}>{profile.executable || 'No executable'}</div>
              <div className="chip-group compact">{(profile.systems ?? []).length === 0 ? <span className="muted-text">No systems selected</span> : (profile.systems ?? []).map(id => <span className="chip static" key={id}>{systemName(id)}</span>)}</div>
            </div>
            <label className="root-toggle"><input type="checkbox" checked={profile.enabled !== false} onChange={event => void act(() => window.lobby.emulators.save({ ...profile, enabled: event.target.checked }), 'Could not update the emulator')} aria-label={'Enable ' + profile.name} /> Enabled</label>
            <button type="button" className="secondary-button" onClick={() => setEditing(isEditing ? null : { profile, isNew: false })}>{isEditing ? 'Close' : 'Edit'}</button>
            {confirming === profile.id
              ? <span className="confirm-group"><span>{profile.source === 'discovered' ? 'Remove? It won’t come back on rescan.' : 'Remove this emulator?'}</span><button type="button" className="secondary-button danger" onClick={() => { setConfirming(null); void act(() => window.lobby.emulators.remove(profile.id), 'Could not remove the emulator') }}>Remove</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
              : <button type="button" className="secondary-button danger" onClick={() => setConfirming(profile.id)}>Remove</button>}
          </div>
          {isEditing && editing && <EmulatorEditor initial={editing.profile} isNew={false} systems={catalog?.systems ?? []} onSaved={list => { setEmulators(list); setEditing(null); say('Emulator saved') }} onCancel={() => setEditing(null)} />}
        </div>
      })}
    </div>
    <div className="settings-card">
      <details className="collapsible"><summary>Ignored emulators ({ignored.length})</summary>
        <p className="muted-text">Removed emulators are not added again by scans. Restore one to let scans find it again.</p>
        {ignored.length === 0 && <div className="empty-inline">Nothing ignored.</div>}
        {ignored.map(item => <div className="root-row" key={item.path}><div className="root-main"><div className="root-path" title={item.path}>{item.path}</div><div className="root-meta"><span>Ignored {formatWhen(item.addedAt)}</span></div></div><button type="button" className="secondary-button" onClick={() => void act(async () => setIgnored(await window.lobby.emulators.unignore(item.path)), 'Could not restore')}>Restore</button></div>)}
      </details>
    </div>
  </>
}
