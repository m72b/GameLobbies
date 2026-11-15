import { useEffect, useState } from 'react'
import { argsSupport, argsText } from '../shared/routes'
import { formatTotal } from './format'
import type { GameRoute, UnlinkPreview } from '../shared/types'

const errorText = (error: unknown, fallback: string) => error instanceof Error ? error.message : typeof error === 'string' ? error : fallback

export type RouteTool = 'name' | 'args' | 'unlink'

/**
 * The game page's tools for one route: its name (display only), its launch options (checked and
 * confirmed in a Windows dialog by Game Lobby; only for games started from their own .exe) and
 * unlinking it into a game of its own (with a preview first).
 */
export function RouteToolPanel({ tool, route, canUnlink, onRename, onArgs, onUnlinkPreview, onUnlink, onClose }: {
  tool: RouteTool
  route: GameRoute
  canUnlink: boolean
  onRename(name: string): Promise<void>
  onArgs(args: string): Promise<void>
  onUnlinkPreview(): Promise<UnlinkPreview>
  onUnlink(): Promise<void>
  onClose(): void
}) {
  const [name, setName] = useState(route.name ?? '')
  const [args, setArgs] = useState(argsText(route.launchArgs))
  const [preview, setPreview] = useState<UnlinkPreview | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const support = argsSupport(route)

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
      ? <form onSubmit={event => { event.preventDefault(); void run(() => onArgs(args), 'Could not save the launch options') }}>
        <label>Launch options<input value={args} maxLength={2000} placeholder="-windowed -width 1920" autoFocus onChange={event => setArgs(event.target.value)} /></label>
        <p className="muted-text">Passed to the game’s .exe as separate arguments, without a command prompt. Quote parts with spaces. Files must be inside the game’s folder. Windows asks you to confirm exactly what will run before anything is saved.</p>
        <div className="settings-actions"><button className="primary-button" disabled={busy}>Save options…</button><button type="button" className="secondary-button" onClick={onClose}>Cancel</button></div>
      </form>
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
    {error && <div className="form-error" role="alert">{error}</div>}
  </div>
}
