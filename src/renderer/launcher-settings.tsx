import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { FolderPlus, ImagePlus } from 'lucide-react'
import type { CustomLauncher, Game, LauncherInput, LauncherStartMode, LauncherTargetKind, LibraryRoot } from '../shared/types'
import { LAUNCHER_ICONS, LAUNCHER_SWATCHES, TARGET_KIND_ORDER, TARGET_PLACEHOLDER, canScanFolders, formatArgv, isHexColor, isLabelOnly, kindLabel, kindsForMode, launcherKinds, launcherRoots, opensFirst, previewArgv, routeLauncherId, sampleTarget, startModeOf, toggleKind } from '../shared/launchers'
import { LauncherIcon } from './launcher-icon'
import { ScanProgress, ScanSummary, alreadyRunning, type LocalScan } from './library-settings'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback
const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path
const formatWhen = (time?: number) => time ? new Date(time).toLocaleString() : 'Never scanned'
const kindHints: Record<LauncherTargetKind, string> = {
  file: 'a file you choose or one found in its games folders (a game’s .exe, a shortcut, a ROM)',
  folder: 'a folder you choose or a game folder found in its games folders',
  identifier: 'a short id you type (a game id, an app name)'
}

interface Draft extends LauncherInput {
  /** Shown only; the path itself is never sent back. */
  executablePath: string
  workingDirPath: string
}

// New launchers with a program are opened first by default: most launchers people sign in to don't
// take a game on their command line.
const blankDraft = (): Draft => ({ name: '', iconKind: 'builtin', iconValue: 'gamepad-2', color: LAUNCHER_SWATCHES[0], argsTemplate: TARGET_PLACEHOLDER, targetKinds: ['file'], workingDir: 'executable', noProgram: false, startMode: 'openFirst', executablePath: '', workingDirPath: '' })
const draftOf = (launcher: CustomLauncher): Draft => ({ id: launcher.id, name: launcher.name, iconKind: launcher.iconKind, iconValue: launcher.iconValue, color: launcher.color, argsTemplate: launcher.argsTemplate, targetKinds: launcherKinds(launcher), workingDir: launcher.workingDir ? 'folder' : 'executable', noProgram: isLabelOnly(launcher), startMode: isLabelOnly(launcher) ? undefined : startModeOf(launcher), executablePath: launcher.executable, workingDirPath: launcher.workingDir ?? '' })
const startModes: Array<{ mode: LauncherStartMode; label: string; hint: string }> = [
  { mode: 'openFirst', label: 'Open it first, then start games directly (for launchers you sign in to)', hint: 'Play opens the launcher on its own, with no arguments, if it isn’t running, and waits until you continue (once it’s open and you’re signed in). Then GameLobbies starts the game from its own file: its .exe or shortcut, or a ROM with its emulator.' },
  { mode: 'through', label: 'Start games through it (it takes the game as an argument)', hint: 'Play runs the launcher once, with the arguments below. Use this only if the launcher’s own documentation says it can start a game from its command line.' }
]

function LauncherEditor({ initial, gameCount, onSaved, onCancel }: { initial: Draft; gameCount: number; onSaved(list: CustomLauncher[]): void; onCancel(): void }) {
  const [draft, setDraft] = useState(initial)
  const [hex, setHex] = useState(initial.color)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const isNew = !initial.id
  const set = (patch: Partial<Draft>) => setDraft(current => ({ ...current, ...patch }))
  const pick = async (action: () => Promise<void>, fallback: string) => {
    setError('')
    try { await action() } catch (caught) { setError(errorText(caught, fallback)) }
  }
  const browseExe = () => pick(async () => { const chosen = await window.lobby.launchers.browseExecutable(); if (chosen) set({ executablePath: chosen, useExecutablePick: true, noProgram: false }) }, 'Could not open the file picker')
  const browseFolder = () => pick(async () => { const chosen = await window.lobby.launchers.browseFolder(); if (chosen) set({ workingDir: 'folder', workingDirPath: chosen, useFolderPick: true }) }, 'Could not open the folder picker')
  const chooseIcon = () => pick(async () => { const name = await window.lobby.launchers.chooseIcon(); if (name) set({ iconKind: 'image', iconValue: name }) }, 'Could not use that image')
  const noProgram = !!draft.noProgram
  const mode: LauncherStartMode = draft.startMode ?? 'openFirst'
  const through = !noProgram && mode === 'through'
  const shownKinds = noProgram ? draft.targetKinds : kindsForMode(draft.targetKinds, mode)
  const argv = useMemo(() => previewArgv(draft.argsTemplate, sampleTarget(draft.targetKinds[0] ?? 'file')), [draft.argsTemplate, draft.targetKinds])
  const program = draft.executablePath ? fileName(draft.executablePath) : 'launcher.exe'
  const colorOk = isHexColor(draft.color)
  // The program can't be added or removed while the launcher has games or games folders.
  const programLocked = !isNew && gameCount > 0
  const submit = async () => {
    if (!colorOk) { setError('Enter a colour like #4f8cff.'); return }
    if (!noProgram && draft.workingDir === 'folder' && !draft.workingDirPath) { setError('Choose the working folder with Browse…, or use the launcher’s folder.'); return }
    setBusy(true); setError('')
    const input: LauncherInput = noProgram
      ? { id: draft.id, name: draft.name.trim(), iconKind: draft.iconKind, iconValue: draft.iconValue, color: draft.color, argsTemplate: '', targetKinds: draft.targetKinds, workingDir: 'executable', noProgram: true }
      : { id: draft.id, name: draft.name.trim(), iconKind: draft.iconKind, iconValue: draft.iconValue, color: draft.color, argsTemplate: draft.argsTemplate, targetKinds: shownKinds, workingDir: draft.workingDir, useExecutablePick: !!draft.useExecutablePick, useFolderPick: draft.workingDir === 'folder' && !!draft.useFolderPick, startMode: mode }
    try { onSaved(await window.lobby.launchers.save(input)) }
    catch (caught) { setError(errorText(caught, 'The launcher could not be saved')) }
    finally { setBusy(false) }
  }
  return <form className="emulator-editor launcher-editor" onSubmit={event => { event.preventDefault(); void submit() }}>
    <h4>{isNew ? 'Add launcher' : 'Edit ' + initial.name}</h4>
    <label className="field-label">Name<input value={draft.name} onChange={event => set({ name: event.target.value })} maxLength={64} placeholder="Playnite, Mod Organizer, My Arcade…" required /></label>

    <div className="field-label">Icon
      <div className="launcher-icon-grid" role="group" aria-label="Built-in icons">
        {LAUNCHER_ICONS.map(name => <button key={name} type="button" className={'launcher-icon-choice' + (draft.iconKind === 'builtin' && draft.iconValue === name ? ' active' : '')} aria-pressed={draft.iconKind === 'builtin' && draft.iconValue === name} title={name} onClick={() => set({ iconKind: 'builtin', iconValue: name })}>
          <LauncherIcon launcher={{ iconKind: 'builtin', iconValue: name, color: colorOk ? draft.color : 'currentColor', name }} size={18} />
        </button>)}
      </div>
      <span className="inline-field">
        <button type="button" className="secondary-button" onClick={() => void chooseIcon()}><ImagePlus size={14} /> Use an image…</button>
        {draft.iconKind === 'image' && <span className="launcher-icon-current"><LauncherIcon launcher={{ ...draft, name: draft.name }} size={22} /> Your image</span>}
      </span>
      <small className="chip-hint">PNG, JPEG, WebP or GIF up to 5 MB. It is copied into GameLobbies’s data folder.</small>
    </div>

    <div className="field-label">Colour
      <span className="inline-field launcher-colors">
        {LAUNCHER_SWATCHES.map(color => <button key={color} type="button" className={'launcher-swatch' + (draft.color.toLowerCase() === color ? ' active' : '')} style={{ background: color }} aria-label={'Colour ' + color} aria-pressed={draft.color.toLowerCase() === color} onClick={() => { set({ color }); setHex(color) }} />)}
        <input className="launcher-hex" value={hex} onChange={event => { setHex(event.target.value); set({ color: event.target.value.trim() }) }} aria-label="Colour as hex" aria-invalid={!colorOk} maxLength={7} spellCheck={false} />
      </span>
      {!colorOk && <small className="chip-hint text-warn">Use a hex colour like #4f8cff.</small>}
    </div>

    <label className="field-label">Program (optional)<span className="inline-field"><input value={noProgram ? '' : draft.executablePath} readOnly onClick={() => { if (!programLocked) void browseExe() }} placeholder={noProgram ? 'No program: each game runs directly' : 'Choose the launcher’s .exe with Browse…'} title="The program is chosen with Browse…, so only programs you picked are run" spellCheck={false} disabled={programLocked && noProgram} />
      <button type="button" className="secondary-button" disabled={programLocked && noProgram} onClick={() => void browseExe()}>Browse…</button>
      {!noProgram && <button type="button" className="secondary-button" disabled={programLocked} onClick={() => set({ noProgram: true, executablePath: '', useExecutablePick: false, argsTemplate: '', workingDir: 'executable' })}>No program</button>}</span>
      <small className="chip-hint">Leave empty to run each game directly; the launcher name is used as the label. With a program, only .exe files on a local drive can be used (no .bat, .cmd, .ps1 or network folders).{programLocked ? ' This can’t change while the launcher has games or games folders.' : ''}</small></label>

    {!noProgram && <div className="field-label">When you play one of its games
      <div className="launcher-modes" role="radiogroup" aria-label="When you play one of its games">
        {startModes.map(option => <label key={option.mode} className={'launcher-mode' + (mode === option.mode ? ' active' : '')}>
          <input type="radio" name="launcher-start-mode" value={option.mode} checked={mode === option.mode} onChange={() => set({ startMode: option.mode, targetKinds: kindsForMode(draft.targetKinds, option.mode) })} />
          <span><b>{option.label}</b><small>{option.hint}</small></span>
        </label>)}
      </div>
      <small className="chip-hint">Changing this asks you to confirm what will run.</small>
    </div>}

    <div className="field-label">Each game may use
      <div className="chip-group" role="group" aria-label="What each game may use">
        {TARGET_KIND_ORDER.filter(kind => noProgram || through || kind !== 'identifier').map(kind => { const on = shownKinds.includes(kind); return <button key={kind} type="button" role="checkbox" aria-checked={on} className={'chip ' + (on ? 'active' : '')} onClick={() => set({ targetKinds: toggleKind(shownKinds, kind) })}>{kindLabel(kind)}</button> })}
      </div>
      <small className="chip-hint">{shownKinds.map(kind => kindLabel(kind) + ': ' + kindHints[kind]).join('. ')}. At least one is needed; kinds your games already use stay on.{noProgram ? ' Without a program, games are found in its games folders or added as files.' : ''}{!noProgram && !through ? ' Games start from their own file, so identifiers aren’t available here, and a game added as a folder can’t start: add it from its .exe or ROM file.' : ''}</small>
    </div>

    {through && <>
      <label className="field-label">Arguments<input value={draft.argsTemplate} onChange={event => set({ argsTemplate: event.target.value })} placeholder={TARGET_PLACEHOLDER} spellCheck={false} maxLength={2048} />
        <small className="chip-hint">Use <code>{TARGET_PLACEHOLDER}</code> for the game, as an argument on its own. Which arguments work depends on the launcher: check its own documentation for its command line (some take something like <code>{'--start {target}'}</code>, many take nothing). It is always passed as one argument, whatever it contains; without it the game is added last. No shell is used, and files named in the arguments must be inside the launcher’s folder.</small></label>
      <div className="launcher-preview" aria-live="polite">
        <span className="detail-label">Preview</span>
        {argv ? <code>{program} {formatArgv(argv)}</code> : <span className="text-warn">{TARGET_PLACEHOLDER} must be an argument on its own (not glued to other text), quotes must be closed, and no other placeholders exist.</span>}
      </div>
    </>}
    {!noProgram && !through && <div className="launcher-preview" aria-live="polite">
      <span className="detail-label">Preview</span>
      <code>{program}</code> <span>on its own (no arguments), then the game from its own file. Arguments aren’t used.</span>
    </div>}

    {!noProgram && <>
      <div className="field-label">Working folder
        <div className="chip-group" role="radiogroup" aria-label="Working folder">
          <button type="button" role="radio" aria-checked={draft.workingDir === 'executable'} className={'chip ' + (draft.workingDir === 'executable' ? 'active' : '')} onClick={() => set({ workingDir: 'executable' })}>The program’s folder</button>
          <button type="button" role="radio" aria-checked={draft.workingDir === 'folder'} className={'chip ' + (draft.workingDir === 'folder' ? 'active' : '')} onClick={() => draft.workingDirPath ? set({ workingDir: 'folder' }) : void browseFolder()}>Another folder…</button>
        </div>
        {draft.workingDir === 'folder' && <span className="inline-field"><input value={draft.workingDirPath} readOnly onClick={() => void browseFolder()} aria-label="Working folder" spellCheck={false} /><button type="button" className="secondary-button" onClick={() => void browseFolder()}>Browse…</button></span>}
      </div>
    </>}

    {error && <div className="form-error" role="alert">{error}</div>}
    <div className="settings-actions"><button type="submit" className="primary-button" disabled={busy || (through && !argv) || (!noProgram && !draft.executablePath)}>{busy ? 'Saving…' : 'Save'}</button><button type="button" className="secondary-button" onClick={onCancel}>Cancel</button></div>
  </form>
}

/** A launcher's games folders: add (native picker), rescan and remove. */
function GamesFolders({ launcher, roots, scan, onRoots, onError }: { launcher: CustomLauncher; roots: LibraryRoot[]; scan: LocalScan; onRoots(next: LibraryRoot[]): void; onError(text: string): void }) {
  const [activeId, setActiveId] = useState<string | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)
  const [notNow, setNotNow] = useState<Set<string>>(() => new Set())
  const [mine, setMine] = useState(false)
  const scanning = activeId !== null || scan.running
  const folders = launcherRoots(roots, launcher.id)
  const rescan = async (id: string, full = false) => {
    setActiveId(id); setMine(true)
    try { scan.setSummary(await window.lobby.roots.rescan(id, { full })); onRoots(await window.lobby.roots.list()) }
    catch (caught) { onError(alreadyRunning(errorText(caught, 'Scan failed'))) }
    finally { setActiveId(null) }
  }
  const add = async () => {
    try {
      const chosen = await window.lobby.launchers.browseGamesFolder(launcher.id)
      if (!chosen) return
      const next = await window.lobby.launchers.addFolder(launcher.id, chosen)
      onRoots(next)
      const added = next.find(root => root.launcherId === launcher.id && !roots.some(old => old.id === root.id))
      if (added) await rescan(added.id)
    } catch (caught) { onError(errorText(caught, 'Could not add this folder')) }
  }
  const remove = async (root: LibraryRoot, mode: 'keep' | 'remove') => {
    setConfirming(null)
    try { onRoots(await window.lobby.roots.remove(root.id, mode)) } catch (caught) { onError(errorText(caught, 'Could not remove the folder')) }
  }
  if (!canScanFolders(launcher)) return <small className="chip-hint">Games folders need a launcher whose games use files or folders, or one without a program.</small>
  return <div className="launcher-folders">
    <div className="card-heading"><div><b>Games folders</b><small className="chip-hint">GameLobbies looks through each folder and its subfolders for PC games (one folder per game, or shortcuts), emulators and ROMs, and adds them {isLabelOnly(launcher) ? 'to run directly, labelled “' + launcher.name + '”' : opensFirst(launcher) ? 'to start directly once ' + launcher.name + ' is open' : 'to run through ' + launcher.name}. Nothing found is run while scanning. Games already in your library get another route instead of a copy.</small></div>
      <button type="button" className="secondary-button" disabled={scanning} onClick={() => void add()}><FolderPlus size={14} /> Add folder…</button></div>
    {folders.length === 0 && <div className="empty-inline">No games folders yet.</div>}
    {folders.map(root => {
      const games = root.lastResult?.games
      return <div className={'root-row' + (root.enabled ? '' : ' disabled')} key={root.id}>
        <div className="root-main">
          <div className="root-path" title={root.path}>{root.path}</div>
          <div className="root-meta">
            <span className={'status-pill ' + (root.online ? 'ok' : 'warn')}>{root.online ? 'Online' : 'Offline'}</span>
            <span>{formatWhen(root.lastScanAt)}</span>
            {root.lastResult && (root.lastResult.ok
              ? games && <span>{games.added} added · {games.routesAdded ?? 0} routes added · {root.lastResult.emulators?.added ?? 0} emulators · {games.needsReview ?? 0} to review</span>
              : <span className="text-warn">Failed: {root.lastResult.error || 'could not be read'}</span>)}
          </div>
          {root.lastResult?.ok && root.lastResult.warning && <div className="inline-warning">{root.lastResult.warning}</div>}
        </div>
        <button type="button" className="secondary-button" disabled={!root.enabled || scanning} onClick={() => void rescan(root.id)}>{activeId === root.id ? 'Scanning…' : 'Rescan'}</button>
        {confirming === root.id
          ? <span className="confirm-group"><span>Remove this folder?</span><button type="button" className="secondary-button" onClick={() => void remove(root, 'keep')} title="The games it added stay in your library">Keep games</button><button type="button" className="secondary-button danger" onClick={() => void remove(root, 'remove')} title="The routes it added are removed, and games left without any">Remove games</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
          : <button type="button" className="secondary-button danger" onClick={() => setConfirming(root.id)}>Remove</button>}
      </div>
    })}
    {mine && scanning && <ScanProgress progress={scan.progress} />}
    {mine && !scanning && scan.summary && <ScanSummary summary={scan.summary} busy={scanning} notNow={notNow} onClose={() => { scan.dismiss(); setMine(false) }} onFullScan={id => void rescan(id, true)} onNotNow={id => setNotNow(current => new Set(current).add(id))} />}
  </div>
}

export function LaunchersSection({ scan, say }: { scan: LocalScan; say(text: string): void }) {
  const [launchers, setLaunchers] = useState<CustomLauncher[]>([])
  const [games, setGames] = useState<Game[]>([])
  const [roots, setRoots] = useState<LibraryRoot[]>([])
  const [editing, setEditing] = useState<{ draft: Draft; id?: string } | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)
  const [error, setError] = useState('')
  const reload = useCallback(() => Promise.all([
    window.lobby.launchers.list().then(setLaunchers),
    window.lobby.library.list().then(setGames),
    window.lobby.roots.list().then(setRoots)
  ]).catch(caught => setError(errorText(caught, 'Could not load the launchers'))), [])
  useEffect(() => {
    void reload()
    const offLaunchers = window.lobby.launchers.onChanged(() => void reload())
    const offLibrary = window.lobby.library.onChanged(() => void reload())
    return () => { offLaunchers(); offLibrary() }
  }, [reload])
  const wasRunning = useRef(false)
  useEffect(() => { if (wasRunning.current && !scan.running) void reload(); wasRunning.current = scan.running }, [scan.running, reload])
  const counts = useMemo(() => {
    const map = new Map<string, number>()
    for (const game of games) {
      const ids = new Set(game.routes.map(routeLauncherId).filter((id): id is string => !!id))
      for (const id of ids) map.set(id, (map.get(id) ?? 0) + 1)
    }
    return map
  }, [games])
  const remove = async (launcher: CustomLauncher, mode: 'keep' | 'remove') => {
    setConfirming(null); setError('')
    try {
      setLaunchers(await window.lobby.launchers.remove(launcher.id, mode))
      say(launcher.name + ' deleted')
      void reload()
    } catch (caught) { setError(errorText(caught, 'Could not delete the launcher')) }
  }
  const saved = (list: CustomLauncher[]) => { setLaunchers(list); setEditing(null); say('Launcher saved') }
  return <div className="settings-card">
    <div className="card-heading"><div><h3>Custom launchers</h3><p>Add your own launchers, such as Playnite, a mod manager, an older store client, or an app folder full of games and emulators. Each one gets its own entry in the side bar and library filter. Add games to it with Add game, or give it games folders to scan. Saving a new launcher, or a new program, start mode or arguments, asks you to confirm exactly what will run.</p></div>
      <button type="button" className="primary-button" onClick={() => setEditing({ draft: blankDraft() })}>Add launcher</button></div>
    {error && <div className="form-error" role="alert">{error}</div>}
    {editing && !editing.id && <LauncherEditor initial={editing.draft} gameCount={0} onSaved={saved} onCancel={() => setEditing(null)} />}
    {launchers.length === 0 && !editing && <div className="empty-inline">No custom launchers yet.</div>}
    {launchers.map(launcher => {
      const count = counts.get(launcher.id) ?? 0
      const folderCount = launcherRoots(roots, launcher.id).length
      const isEditing = editing?.id === launcher.id
      return <div className="emu-item" key={launcher.id}>
        <div className="emu-row">
          <span className="store-badge small"><LauncherIcon launcher={launcher} size={14} /></span>
          <div className="root-main">
            <div className="emu-name"><b>{launcher.name}</b>
              {launcherKinds(launcher).map(kind => <span className="status-pill" key={kind}>{kindLabel(kind)}</span>)}
              <span className="status-pill">{count} game{count === 1 ? '' : 's'}</span>
              {folderCount > 0 && <span className="status-pill">{folderCount} folder{folderCount === 1 ? '' : 's'}</span>}
              {isLabelOnly(launcher) && <span className="status-pill">No program</span>}
              {!launcher.available && <span className="status-pill warn">Program not found</span>}
            </div>
            <div className="root-path" title={launcher.executable}>{isLabelOnly(launcher) ? 'Games run directly; “' + launcher.name + '” is their label.' : opensFirst(launcher) ? launcher.executable + ' (opened first; games start from their own file)' : launcher.executable + (launcher.argsTemplate ? ' ' + launcher.argsTemplate : '')}</div>
          </div>
          <button type="button" className="secondary-button" onClick={() => setEditing(isEditing ? null : { draft: draftOf(launcher), id: launcher.id })}>{isEditing ? 'Close' : 'Edit'}</button>
          {confirming === launcher.id
            ? count === 0 && folderCount === 0
              ? <span className="confirm-group"><span>Delete this launcher?</span><button type="button" className="secondary-button danger" onClick={() => void remove(launcher, 'keep')}>Delete</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
              : <span className="confirm-group"><span>{count} game{count === 1 ? ' uses' : 's use'} it{folderCount > 0 ? ' (its games folders go too)' : ''}.</span><button type="button" className="secondary-button" onClick={() => void remove(launcher, 'keep')} title="The games stay in your library, without this launcher">Keep games</button><button type="button" className="secondary-button danger" onClick={() => void remove(launcher, 'remove')} title="Its routes are removed, and games left without any">Remove games</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
            : <button type="button" className="secondary-button danger" onClick={() => setConfirming(launcher.id)}>Delete</button>}
        </div>
        {isEditing && editing && <LauncherEditor initial={editing.draft} gameCount={count + folderCount} onSaved={saved} onCancel={() => setEditing(null)} />}
        <GamesFolders launcher={launcher} roots={roots} scan={scan} onRoots={setRoots} onError={setError} />
      </div>
    })}
  </div>
}
