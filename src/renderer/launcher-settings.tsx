import { useCallback, useEffect, useMemo, useState } from 'react'
import { ImagePlus } from 'lucide-react'
import type { CustomLauncher, Game, LauncherInput, LauncherTargetKind } from '../shared/types'
import { LAUNCHER_ICONS, LAUNCHER_SWATCHES, TARGET_PLACEHOLDER, formatArgv, isHexColor, launcherIdOf, previewArgv, sampleTarget } from '../shared/launchers'
import { LauncherIcon } from './launcher-icon'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback
const fileName = (path: string) => path.split(/[\\/]/).filter(Boolean).pop() ?? path
const targetKinds: Array<[LauncherTargetKind, string, string]> = [
  ['file', 'File', 'Each game is a file you choose (a ROM, a profile, a shortcut file).'],
  ['folder', 'Folder', 'Each game is a folder you choose (a game or mod folder).'],
  ['identifier', 'Identifier', 'Each game is a short id you type (a game id, an app name).']
]

interface Draft extends LauncherInput {
  /** Shown only; the path itself is never sent back. */
  executablePath: string
  workingDirPath: string
}

const blankDraft = (): Draft => ({ name: '', iconKind: 'builtin', iconValue: 'gamepad-2', color: LAUNCHER_SWATCHES[0], argsTemplate: TARGET_PLACEHOLDER, targetKind: 'identifier', workingDir: 'executable', executablePath: '', workingDirPath: '' })
const draftOf = (launcher: CustomLauncher): Draft => ({ id: launcher.id, name: launcher.name, iconKind: launcher.iconKind, iconValue: launcher.iconValue, color: launcher.color, argsTemplate: launcher.argsTemplate, targetKind: launcher.targetKind, workingDir: launcher.workingDir ? 'folder' : 'executable', executablePath: launcher.executable, workingDirPath: launcher.workingDir ?? '' })

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
  const browseExe = () => pick(async () => { const chosen = await window.lobby.launchers.browseExecutable(); if (chosen) set({ executablePath: chosen, useExecutablePick: true }) }, 'Could not open the file picker')
  const browseFolder = () => pick(async () => { const chosen = await window.lobby.launchers.browseFolder(); if (chosen) set({ workingDir: 'folder', workingDirPath: chosen, useFolderPick: true }) }, 'Could not open the folder picker')
  const chooseIcon = () => pick(async () => { const name = await window.lobby.launchers.chooseIcon(); if (name) set({ iconKind: 'image', iconValue: name }) }, 'Could not use that image')
  const argv = useMemo(() => previewArgv(draft.argsTemplate, sampleTarget(draft.targetKind)), [draft.argsTemplate, draft.targetKind])
  const program = draft.executablePath ? fileName(draft.executablePath) : 'launcher.exe'
  const colorOk = isHexColor(draft.color)
  const submit = async () => {
    if (!colorOk) { setError('Enter a colour like #4f8cff.'); return }
    if (draft.workingDir === 'folder' && !draft.workingDirPath) { setError('Choose the working folder with Browse…, or use the launcher’s folder.'); return }
    setBusy(true); setError('')
    const input: LauncherInput = { id: draft.id, name: draft.name.trim(), iconKind: draft.iconKind, iconValue: draft.iconValue, color: draft.color, argsTemplate: draft.argsTemplate, targetKind: draft.targetKind, workingDir: draft.workingDir, useExecutablePick: !!draft.useExecutablePick, useFolderPick: draft.workingDir === 'folder' && !!draft.useFolderPick }
    try { onSaved(await window.lobby.launchers.save(input)) }
    catch (caught) { setError(errorText(caught, 'The launcher could not be saved')) }
    finally { setBusy(false) }
  }
  return <form className="emulator-editor launcher-editor" onSubmit={event => { event.preventDefault(); void submit() }}>
    <h4>{isNew ? 'Add launcher' : 'Edit ' + initial.name}</h4>
    <label className="field-label">Name<input value={draft.name} onChange={event => set({ name: event.target.value })} maxLength={64} placeholder="Playnite, Mod Organizer…" required /></label>

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
      <small className="chip-hint">PNG, JPEG, WebP or GIF up to 5 MB. It is copied into Game Lobby’s data folder.</small>
    </div>

    <div className="field-label">Colour
      <span className="inline-field launcher-colors">
        {LAUNCHER_SWATCHES.map(color => <button key={color} type="button" className={'launcher-swatch' + (draft.color.toLowerCase() === color ? ' active' : '')} style={{ background: color }} aria-label={'Colour ' + color} aria-pressed={draft.color.toLowerCase() === color} onClick={() => { set({ color }); setHex(color) }} />)}
        <input className="launcher-hex" value={hex} onChange={event => { setHex(event.target.value); set({ color: event.target.value.trim() }) }} aria-label="Colour as hex" aria-invalid={!colorOk} maxLength={7} spellCheck={false} />
      </span>
      {!colorOk && <small className="chip-hint text-warn">Use a hex colour like #4f8cff.</small>}
    </div>

    <label className="field-label">Program<span className="inline-field"><input value={draft.executablePath} readOnly onClick={() => void browseExe()} placeholder="Choose the launcher’s .exe with Browse…" title="The program is chosen with Browse…, so only programs you picked are run" spellCheck={false} required /><button type="button" className="secondary-button" onClick={() => void browseExe()}>Browse…</button></span>
      <small className="chip-hint">Only .exe files on a local drive. Script files (.bat, .cmd, .ps1) and network folders can’t be used.</small></label>

    <div className="field-label">Each game uses
      <div className="chip-group" role="radiogroup" aria-label="What each game uses">
        {targetKinds.map(([kind, label]) => <button key={kind} type="button" role="radio" aria-checked={draft.targetKind === kind} className={'chip ' + (draft.targetKind === kind ? 'active' : '')} disabled={gameCount > 0 && initial.targetKind !== kind} onClick={() => set({ targetKind: kind })}>{label}</button>)}
      </div>
      <small className="chip-hint">{targetKinds.find(item => item[0] === draft.targetKind)?.[2]}{gameCount > 0 ? ' This can’t change while the launcher has games.' : ''}</small>
    </div>

    <label className="field-label">Arguments<input value={draft.argsTemplate} onChange={event => set({ argsTemplate: event.target.value })} placeholder={TARGET_PLACEHOLDER} spellCheck={false} maxLength={2048} />
      <small className="chip-hint">Use <code>{TARGET_PLACEHOLDER}</code> for the game, as an argument on its own, e.g. <code>{'--start {target}'}</code>. It is always passed as one argument, whatever it contains; without it the game is added last. No shell is used, and files named in the arguments must be inside the launcher’s folder.</small></label>
    <div className="launcher-preview" aria-live="polite">
      <span className="detail-label">Preview</span>
      {argv ? <code>{program} {formatArgv(argv)}</code> : <span className="text-warn">{TARGET_PLACEHOLDER} must be an argument on its own (not glued to other text), quotes must be closed, and no other placeholders exist.</span>}
    </div>

    <div className="field-label">Working folder
      <div className="chip-group" role="radiogroup" aria-label="Working folder">
        <button type="button" role="radio" aria-checked={draft.workingDir === 'executable'} className={'chip ' + (draft.workingDir === 'executable' ? 'active' : '')} onClick={() => set({ workingDir: 'executable' })}>The program’s folder</button>
        <button type="button" role="radio" aria-checked={draft.workingDir === 'folder'} className={'chip ' + (draft.workingDir === 'folder' ? 'active' : '')} onClick={() => draft.workingDirPath ? set({ workingDir: 'folder' }) : void browseFolder()}>Another folder…</button>
      </div>
      {draft.workingDir === 'folder' && <span className="inline-field"><input value={draft.workingDirPath} readOnly onClick={() => void browseFolder()} aria-label="Working folder" spellCheck={false} /><button type="button" className="secondary-button" onClick={() => void browseFolder()}>Browse…</button></span>}
    </div>

    {error && <div className="form-error" role="alert">{error}</div>}
    <div className="settings-actions"><button type="submit" className="primary-button" disabled={busy || !argv}>{busy ? 'Saving…' : 'Save'}</button><button type="button" className="secondary-button" onClick={onCancel}>Cancel</button></div>
  </form>
}

export function LaunchersSection({ say }: { say(text: string): void }) {
  const [launchers, setLaunchers] = useState<CustomLauncher[]>([])
  const [games, setGames] = useState<Game[]>([])
  const [editing, setEditing] = useState<{ draft: Draft; id?: string } | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)
  const [error, setError] = useState('')
  const reload = useCallback(() => Promise.all([
    window.lobby.launchers.list().then(setLaunchers),
    window.lobby.library.list().then(setGames)
  ]).catch(caught => setError(errorText(caught, 'Could not load the launchers'))), [])
  useEffect(() => {
    void reload()
    const offLaunchers = window.lobby.launchers.onChanged(() => void reload())
    const offLibrary = window.lobby.library.onChanged(() => void reload())
    return () => { offLaunchers(); offLibrary() }
  }, [reload])
  const counts = useMemo(() => {
    const map = new Map<string, number>()
    for (const game of games) {
      const ids = new Set(game.routes.map(launcherIdOf).filter((id): id is string => !!id))
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
    <div className="card-heading"><div><h3>Custom launchers</h3><p>Add your own launchers, such as Playnite, a mod manager or an older store client. Each one gets its own entry in the side bar and library filter, and you can add games to it with Add game. Saving a new launcher, or a new program or arguments, asks you to confirm exactly what will run.</p></div>
      <button type="button" className="primary-button" onClick={() => setEditing({ draft: blankDraft() })}>Add launcher</button></div>
    {error && <div className="form-error" role="alert">{error}</div>}
    {editing && !editing.id && <LauncherEditor initial={editing.draft} gameCount={0} onSaved={saved} onCancel={() => setEditing(null)} />}
    {launchers.length === 0 && !editing && <div className="empty-inline">No custom launchers yet.</div>}
    {launchers.map(launcher => {
      const count = counts.get(launcher.id) ?? 0
      const isEditing = editing?.id === launcher.id
      return <div className="emu-item" key={launcher.id}>
        <div className="emu-row">
          <span className="store-badge small"><LauncherIcon launcher={launcher} size={14} /></span>
          <div className="root-main">
            <div className="emu-name"><b>{launcher.name}</b>
              <span className="status-pill">{targetKinds.find(item => item[0] === launcher.targetKind)?.[1]}</span>
              <span className="status-pill">{count} game{count === 1 ? '' : 's'}</span>
              {!launcher.available && <span className="status-pill warn">Program not found</span>}
            </div>
            <div className="root-path" title={launcher.executable}>{launcher.executable}{launcher.argsTemplate ? ' ' + launcher.argsTemplate : ''}</div>
          </div>
          <button type="button" className="secondary-button" onClick={() => setEditing(isEditing ? null : { draft: draftOf(launcher), id: launcher.id })}>{isEditing ? 'Close' : 'Edit'}</button>
          {confirming === launcher.id
            ? count === 0
              ? <span className="confirm-group"><span>Delete this launcher?</span><button type="button" className="secondary-button danger" onClick={() => void remove(launcher, 'keep')}>Delete</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
              : <span className="confirm-group"><span>{count} game{count === 1 ? ' uses' : 's use'} it.</span><button type="button" className="secondary-button" onClick={() => void remove(launcher, 'keep')} title="The games stay in your library as Custom games, without this launcher">Keep games</button><button type="button" className="secondary-button danger" onClick={() => void remove(launcher, 'remove')} title="Games that only start through this launcher are removed from your library">Remove games</button><button type="button" className="secondary-button" onClick={() => setConfirming(null)}>Cancel</button></span>
            : <button type="button" className="secondary-button danger" onClick={() => setConfirming(launcher.id)}>Delete</button>}
        </div>
        {isEditing && editing && <LauncherEditor initial={editing.draft} gameCount={count} onSaved={saved} onCancel={() => setEditing(null)} />}
      </div>
    })}
  </div>
}
