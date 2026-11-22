import { useEffect, useId, useState } from 'react'
import { ExternalLink } from 'lucide-react'
import { LAUNCH_OPTIONS_HELP, argsSupport, argsText, type ArgsSupport } from '../shared/routes'
import { formatTotal } from './format'
import type { ArgsPreview, CustomLauncher, GameRoute, UnlinkPreview } from '../shared/types'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

export type RouteTool = 'name' | 'args' | 'unlink'

export function commandText(argv: readonly string[]): string {
  return argsText(JSON.stringify(argv))
}

const fileName = (path: string) => path.split(/[\\/]/).pop() || path

type Check = { state: 'idle' } | { state: 'checking' } | { state: 'ok'; preview: ArgsPreview } | { state: 'error'; message: string }

/**
 * Asks Rust what `text` would run while the user types (debounced; nothing is saved): the reason
 * it would be refused, or the program and whole argument list. Rust decides; this is feedback only.
 */
function useOptionsCheck(text: string, check: (text: string) => Promise<ArgsPreview>, key: string): Check {
  const [result, setResult] = useState<Check>({ state: 'idle' })
  useEffect(() => {
    if (!text.trim()) { setResult({ state: 'idle' }); return }
    let live = true
    setResult(previous => previous.state === 'idle' ? { state: 'checking' } : previous)
    const timer = window.setTimeout(() => {
      check(text)
        .then(preview => { if (live) setResult({ state: 'ok', preview }) })
        .catch(caught => { if (live) setResult({ state: 'error', message: errorText(caught, 'These launch options can’t be used.') }) })
    }, 350)
    return () => { live = false; window.clearTimeout(timer) }
  }, [text, key]) // eslint-disable-line react-hooks/exhaustive-deps
  return result
}

/**
 * The launch options text box with its live feedback (Rust's refusal reason, or what will run), a
 * placeholder example and a short hint with a link to the user guide. Used on the game page and
 * when adding a game to a launcher.
 */
export function LaunchOptionsField({ value, onChange, check, checkKey, launcher, autoFocus, disabled }: {
  value: string
  onChange(value: string): void
  check(text: string): Promise<ArgsPreview>
  /** Changes when what the options are checked against changes (route, launcher, target). */
  checkKey: string
  launcher?: string
  autoFocus?: boolean
  disabled?: boolean
}) {
  const id = useId()
  const result = useOptionsCheck(value, check, checkKey)
  const invalid = result.state === 'error'
  return <div className="lo-field">
    <label htmlFor={id + '-input'}>Launch options <span className="lo-optional">(optional)</span></label>
    <input id={id + '-input'} value={value} maxLength={1000} placeholder="For example -windowed -nosplash" autoFocus={autoFocus} disabled={disabled}
      spellCheck={false} autoComplete="off" aria-invalid={invalid} aria-describedby={id + '-feedback ' + id + '-hint'} onChange={event => onChange(event.target.value)} />
    <div id={id + '-feedback'} aria-live="polite">
      {result.state === 'error' && <div className="lo-error">{result.message}</div>}
      {result.state === 'ok' && result.preview.argv.length > 0 && <div className="lo-preview" title={result.preview.program + ' ' + commandText(result.preview.argv)}>
        <span>Runs</span><code>{fileName(result.preview.program)} {commandText(result.preview.argv)}</code>
      </div>}
    </div>
    <p id={id + '-hint'} className="lo-hint">
      {launcher
        ? <>Added after {launcher}’s own arguments and the game. Files must be inside {launcher}’s folder.</>
        : <>Passed to the game’s .exe. Files must be inside the game’s folder.</>}
      {' '}Separate options with spaces and quote parts that contain spaces. No command prompt is used, and Windows shows exactly what will run before anything is saved.
      {' '}<a className="lo-help" href={LAUNCH_OPTIONS_HELP}>How launch options work <ExternalLink size={10} /></a>
    </p>
  </div>
}

/**
 * Editing one route's launch options: the field, a "Needs confirmation" notice for stored options
 * Play won't use yet, and Save / Confirm / Remove. Rust checks the text, shows a native
 * confirmation and saves; the renderer only sends the text.
 */
export function LaunchOptionsEditor({ route, support, pending, autoFocus, onSave, onCheck, onClose }: {
  route: GameRoute
  support: Extract<ArgsSupport, { ok: true }>
  /** The stored options need confirming before they run (Rust decides). */
  pending: boolean
  autoFocus?: boolean
  onSave(args: string): Promise<void>
  onCheck(args: string): Promise<ArgsPreview>
  onClose(): void
}) {
  const stored = argsText(route.launchArgs)
  const [args, setArgs] = useState(stored)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const unchanged = args.trim() === stored.trim()
  const confirming = pending && unchanged && !!stored
  const save = async (text: string) => {
    setBusy(true); setError('')
    try { await onSave(text); onClose() }
    catch (caught) { setError(errorText(caught, 'Could not save the launch options')) }
    finally { setBusy(false) }
  }
  return <form className="lo-editor" onSubmit={event => { event.preventDefault(); void save(args) }}>
    <LaunchOptionsField value={args} onChange={setArgs} check={onCheck} checkKey={route.sourceId + '|' + route.launchTarget} launcher={support.launcher} autoFocus={autoFocus} disabled={busy} />
    {confirming && <div className="lo-pending" role="status"><b>Needs confirmation.</b> These options came from a backup or an older version, or {support.launcher ? support.launcher + '’s program, arguments or working folder, or the game’s target,' : 'the game’s .exe'} changed, so Play won’t use them until you confirm them.</div>}
    {error && <div className="form-error" role="alert">{error}</div>}
    <div className="settings-actions">
      <button className="primary-button" disabled={busy || (unchanged && !confirming)}>{confirming ? 'Confirm options…' : args.trim() ? 'Save options…' : 'Remove options'}</button>
      {stored && args.trim() && <button type="button" className="secondary-button" disabled={busy} onClick={() => void save('')}>Remove</button>}
      <button type="button" className="secondary-button" onClick={onClose}>Cancel</button>
    </div>
  </form>
}

export function LaunchOptionsDialog({ title, routeLabel, ...editor }: Parameters<typeof LaunchOptionsEditor>[0] & { title: string; routeLabel: string }) {
  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget) editor.onClose() }}>
    <div className="modal lo-dialog" role="dialog" aria-modal="true" aria-labelledby="lo-dialog-title">
      <div className="modal-head"><h2 id="lo-dialog-title">Launch options</h2></div>
      <p className="lo-for">For <b>{title}</b> when played from <b>{routeLabel}</b>.</p>
      <LaunchOptionsEditor {...editor} autoFocus />
    </div>
  </div>
}

/**
 * The game page's tools for one route: its name (display only), its launch options (checked and
 * confirmed in a Windows dialog by GameLobbies; for games started from their own .exe and for
 * custom launchers with a program) and unlinking it into a game of its own (with a preview first).
 */
export function RouteToolPanel({ tool, route, launchers, canUnlink, onRename, onArgs, onArgsPending, onArgsCheck, onUnlinkPreview, onUnlink, onClose }: {
  tool: RouteTool
  route: GameRoute
  launchers: readonly CustomLauncher[]
  canUnlink: boolean
  onRename(name: string): Promise<void>
  onArgs(args: string): Promise<void>
  /** Whether the stored options need confirming before they run (Rust decides). */
  onArgsPending(): Promise<boolean>
  onArgsCheck(args: string): Promise<ArgsPreview>
  onUnlinkPreview(): Promise<UnlinkPreview>
  onUnlink(): Promise<void>
  onClose(): void
}) {
  const [name, setName] = useState(route.name ?? '')
  const [preview, setPreview] = useState<UnlinkPreview | null>(null)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const support = argsSupport(route, launchers)

  useEffect(() => {
    if (tool !== 'args' || !route.launchArgs) { setPending(false); return }
    let live = true
    onArgsPending().then(next => { if (live) setPending(next) }).catch(() => { if (live) setPending(false) })
    return () => { live = false }
  }, [tool, route.sourceId, route.launchTarget, route.launchArgs, launchers]) // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (tool !== 'unlink' || !canUnlink) return
    let live = true
    setPreview(null); setError('')
    onUnlinkPreview().then(next => { if (live) setPreview(next) }).catch(caught => { if (live) setError(errorText(caught, 'Could not prepare this')) })
    return () => { live = false }
  }, [tool, route.sourceId, route.launchTarget]) // eslint-disable-line react-hooks/exhaustive-deps

  const run = async (action: () => Promise<void>, fallback: string) => {
    setBusy(true); setError('')
    try { await action(); onClose() }
    catch (caught) { setError(errorText(caught, fallback)) }
    finally { setBusy(false) }
  }

  return <div className="gp-route-tool">
    {tool === 'name' && <form onSubmit={event => { event.preventDefault(); void run(() => onRename(name), 'Could not rename this route') }}>
      <label>Route name<input value={name} maxLength={60} placeholder="For example Java Edition" autoFocus onChange={event => setName(event.target.value)} /></label>
      <p className="muted-text">Shown after the store under Play from. It only changes how the route reads, never what it starts. Leave empty for none.</p>
      <div className="settings-actions"><button className="primary-button" disabled={busy}>Save name</button><button type="button" className="secondary-button" onClick={onClose}>Cancel</button></div>
    </form>}
    {tool === 'args' && (support.ok
      ? <LaunchOptionsEditor route={route} support={support} pending={pending} autoFocus onSave={onArgs} onCheck={onArgsCheck} onClose={onClose} />
      : <><p className="muted-text">{support.reason}</p><div className="settings-actions"><button type="button" className="secondary-button" onClick={onClose}>Close</button></div></>)}
    {tool === 'unlink' && (!canUnlink
      ? <><p className="muted-text">This is the game’s only launch route, so there is nothing to unlink.</p><div className="settings-actions"><button type="button" className="secondary-button" onClick={onClose}>Close</button></div></>
      : preview
        ? <>
          <p className="muted-text">{preview.restores
            ? <>This route came from <b>{preview.title}</b> when the games were linked. It becomes its own game again with its title, cover, favourite, notes and other details, {preview.sessions} session{preview.sessions === 1 ? '' : 's'} and {formatTotal(preview.playtimeSeconds)} of playtime it brought. Sessions played since linking stay here.</>
            : <>It becomes a new game, <b>{preview.title}</b>, with the store’s cover. Your notes, sessions and playtime stay with this game.</>}</p>
          <ul className="review-games"><li>Moves: {preview.routes.join(', ')}</li><li>Stays: {preview.remaining.join(', ')}</li></ul>
          <div className="settings-actions"><button type="button" className="primary-button" disabled={busy} onClick={() => void run(onUnlink, 'Could not unlink this route')}>Unlink</button><button type="button" className="secondary-button" onClick={onClose}>Cancel</button></div>
        </>
        : !error && <p className="muted-text">Preparing…</p>)}
    {tool !== 'args' && error && <div className="form-error" role="alert">{error}</div>}
  </div>
}
