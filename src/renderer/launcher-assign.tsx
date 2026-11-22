import { useEffect, useMemo, useState } from 'react'
import { Check, Link2, X } from 'lucide-react'
import { LauncherIcon } from './launcher-icon'
import { assignTargetLabel, isLabelOnly, linkedMessage, playsWithText } from '../shared/launchers'
import type { AssignmentOption, CustomLauncher, Game } from '../shared/types'

function useAssignments(game: Game, launchers: CustomLauncher[], libraryVersion = 0): AssignmentOption[] | null {
  const [options, setOptions] = useState<AssignmentOption[] | null>(null)
  const routesKey = game.routes.map(route => route.sourceId + '|' + route.launchTarget + '|' + (route.launcherId ?? '') + '|' + (route.assigned ? 1 : 0)).join('\n')
  useEffect(() => {
    let live = true
    window.lobby.launchers.assignments(game.id).then(next => { if (live) setOptions(next) }).catch(() => { if (live) setOptions([]) })
    return () => { live = false }
  }, [game.id, routesKey, libraryVersion, launchers])
  return options
}

/**
 * The game page's LAUNCHERS block: the custom launchers this game shows under (the ones linked on
 * the game page can be removed here), and "Link to launcher…", which opens {@link LinkLauncherDialog}.
 * Launch options for a launcher route are set later with the route's own Launch options control.
 */
export function LauncherBlock({ game, launchers, libraryVersion, onOpenLink, onUnassign }: {
  game: Game
  launchers: CustomLauncher[]
  libraryVersion: number
  onOpenLink(): void
  onUnassign(launcherId: string): Promise<void>
}) {
  const options = useAssignments(game, launchers, libraryVersion) ?? []
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => { setError('') }, [game.id])

  const byId = useMemo(() => new Map(launchers.map(launcher => [launcher.id, launcher])), [launchers])
  const listed = options.filter(option => option.listed && byId.has(option.launcherId))
  const canLink = options.some(option => !option.listed && byId.has(option.launcherId))

  const remove = async (launcherId: string) => {
    setBusy(true); setError('')
    try { await onUnassign(launcherId) } catch (e) { setError(e instanceof Error ? e.message : 'Something went wrong') } finally { setBusy(false) }
  }

  if (launchers.length === 0) return null
  return <div className="gp-side-block gp-launchers"><div className="detail-label">LAUNCHERS</div>
    {listed.length === 0 && <div className="muted-text">Not in any custom launcher.</div>}
    {listed.map(option => {
      const launcher = byId.get(option.launcherId)!
      return <div className="gp-route-row" key={option.launcherId}>
        <span className="store-badge small"><LauncherIcon launcher={launcher} /></span>
        <span className="route-name"><span className="route-label">{launcher.name}</span><small>{option.assigned ? (isLabelOnly(launcher) ? 'Listed here' : playsWithText(launcher)) : 'Added in the launcher'}</small></span>
        {option.assigned && <button type="button" className="gp-launcher-remove" disabled={busy} title={'Unlink from ' + launcher.name} aria-label={'Unlink from ' + launcher.name} onClick={() => void remove(option.launcherId)}><X size={13} /></button>}
      </div>
    })}
    {canLink && <div className="gp-launcher-add"><button type="button" className="secondary-button" onClick={onOpenLink}><Link2 size={14} /> Link to launcher…</button></div>}
    {error && <div className="form-error" role="alert">{error}</div>}
  </div>
}

/**
 * "Link to launcher": every custom launcher, and one click links the game to it. Only ids go to
 * Rust (`launchers.link`): it picks the target from the game's own routes and asks in a native
 * dialog only when the launcher's command wasn't confirmed on this PC yet. Launchers that can't
 * take the game are shown disabled with the reason; linked ones can be unlinked. When the game
 * offers the launcher several targets, "Choose which file…" picks one (`launchers.assign`).
 */
export function LinkLauncherDialog({ game, launchers, onGames, onClose }: {
  game: Game
  launchers: CustomLauncher[]
  /** The library after a change. */
  onGames(games: Game[]): void
  onClose(): void
}) {
  const options = useAssignments(game, launchers)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [done, setDone] = useState<{ launcherId: string; message: string } | null>(null)
  const [choosing, setChoosing] = useState<{ launcherId: string; index: number } | null>(null)
  const byId = useMemo(() => new Map(launchers.map(launcher => [launcher.id, launcher])), [launchers])
  const shown = (options ?? []).filter(option => byId.has(option.launcherId))

  const run = async (work: () => Promise<void>) => {
    setBusy(true); setError('')
    try { await work() } catch (e) { setError(e instanceof Error ? e.message : 'Something went wrong') } finally { setBusy(false) }
  }
  const linked = (launcher: CustomLauncher) => { setChoosing(null); setDone({ launcherId: launcher.id, message: linkedMessage(launcher) }) }
  const link = (launcher: CustomLauncher) => run(async () => {
    const result = await window.lobby.launchers.link(game.id, launcher.id)
    onGames(result.games)
    linked(launcher)
  })
  const linkChosen = (launcher: CustomLauncher, option: AssignmentOption, index: number) => run(async () => {
    const target = option.targets[index]
    onGames(await window.lobby.launchers.assign(game.id, launcher.id, target?.routeIndex, target?.kind))
    linked(launcher)
  })
  const unlink = (launcherId: string) => run(async () => {
    onGames(await window.lobby.launchers.unassign(game.id, launcherId))
    setDone(null)
  })

  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget && !busy) onClose() }}>
    <div className="modal link-modal link-launcher-modal" role="dialog" aria-modal="true" aria-labelledby="link-launcher-title">
      <div className="modal-head"><h2 id="link-launcher-title">Link to launcher</h2><button type="button" aria-label="Close" disabled={busy} onClick={onClose}><X size={17} /></button></div>
      <p>Choose the launcher that starts <b>{game.title}</b>. Its other ways to start stay under Play from.</p>
      {done && <div className="link-launcher-done" role="status">
        <Check size={15} /><span>{done.message}</span>
        <button type="button" className="link-button" disabled={busy} onClick={() => void unlink(done.launcherId)}>Undo</button>
      </div>}
      <div className="link-game-list link-launcher-list">
        {options === null && <div className="empty-inline">Loading launchers…</div>}
        {options !== null && shown.length === 0 && <div className="empty-inline">No custom launchers yet. Add one in Settings → Custom launchers.</div>}
        {shown.map(option => {
          const launcher = byId.get(option.launcherId)!
          const ownGame = option.listed && !option.assigned
          const blocked = !option.listed && !!option.problem
          const status = option.assigned ? (isLabelOnly(launcher) ? 'Listed here' : playsWithText(launcher))
            : ownGame ? 'Added in the launcher'
            : option.problem ? option.problem
            : isLabelOnly(launcher) ? 'Lists the game here; it keeps starting the same way'
            : option.targets[0] ? 'Gets ' + assignTargetLabel(option.targets[0], game.routes) : ''
          const picking = choosing?.launcherId === option.launcherId ? choosing : null
          const style = { '--launcher-color': launcher.color } as React.CSSProperties
          return <div className={'link-launcher-row' + (blocked || ownGame ? ' disabled' : '') + (option.assigned ? ' linked' : '')} key={option.launcherId} style={style}>
            <div className="link-launcher-line">
              <span className="link-launcher-icon"><LauncherIcon launcher={launcher} size={17} /></span>
              <span className="link-launcher-text"><b>{launcher.name}</b><small title={status}>{status}</small></span>
              {option.assigned
                ? <><span className="link-launcher-state">Linked</span><button type="button" className="secondary-button" disabled={busy} onClick={() => void unlink(option.launcherId)}>Unlink</button></>
                : !option.listed && !option.problem && <button type="button" className="primary-button" disabled={busy} aria-label={'Link ' + game.title + ' to ' + launcher.name} onClick={() => void link(launcher)}>Link</button>}
            </div>
            {!option.listed && !option.problem && option.targets.length > 1 && !picking && <button type="button" className="link-button link-launcher-choose" disabled={busy} onClick={() => setChoosing({ launcherId: option.launcherId, index: 0 })}>Choose which file…</button>}
            {picking && <div className="link-launcher-pick">
              <select aria-label={'What ' + launcher.name + ' gets'} value={picking.index} disabled={busy} onChange={event => setChoosing({ launcherId: option.launcherId, index: Number(event.target.value) })}>
                {option.targets.map((item, i) => <option key={item.kind + item.target} value={i} title={item.target}>{assignTargetLabel(item, game.routes)}</option>)}
              </select>
              <button type="button" className="secondary-button" disabled={busy} onClick={() => void linkChosen(launcher, option, picking.index)}>Link with this</button>
              <button type="button" className="link-button" disabled={busy} onClick={() => setChoosing(null)}>Cancel</button>
            </div>}
          </div>
        })}
      </div>
      {error && <div className="form-error" role="alert">{error}</div>}
      <p className="link-launcher-note">Launch options for the launcher can be added afterwards with Launch options under Play from.</p>
      <div className="modal-actions"><button type="button" className="primary-button" disabled={busy} onClick={onClose}>Done</button></div>
    </div>
  </div>
}

export interface WaitingLaunch { game: Game; pendingId: string; launcherName: string }

/**
 * "Waiting for <launcher>": an "open first" launcher's program was started on its own (no
 * arguments). Continue confirms the pending launch: Rust then starts the game directly from its own
 * file, checked again; the renderer sends only the game and pending ids. The launcher's process is
 * watched to show when it is open.
 */
export function LauncherWaitDialog({ wait, onClose, onStarted }: { wait: WaitingLaunch; onClose(): void; onStarted(): void }) {
  const [running, setRunning] = useState(false)
  const [expired, setExpired] = useState(false)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => {
    let live = true
    const check = () => window.lobby.library.pendingLaunch(wait.pendingId).then(status => {
      if (!live) return
      setRunning(status.launcherRunning)
      setExpired(!status.pending)
    }).catch(() => {})
    void check()
    const timer = window.setInterval(() => void check(), 1500)
    return () => { live = false; window.clearInterval(timer) }
  }, [wait.pendingId])
  const cancel = () => { void window.lobby.library.cancelLaunch(wait.pendingId).catch(() => {}); onClose() }
  const go = async () => {
    setBusy(true); setError('')
    try {
      await window.lobby.library.continueLaunch(wait.game.id, wait.pendingId)
      onStarted()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not launch the game')
      setBusy(false)
    }
  }
  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget && !busy) cancel() }}>
    <div className="modal launcher-wait" role="dialog" aria-modal="true" aria-labelledby="launcher-wait-title">
      <div className="modal-head"><h2 id="launcher-wait-title">Waiting for {wait.launcherName}</h2></div>
      <p>Continue when it’s open and you’re signed in. Game Lobby then starts <b>{wait.game.title}</b> directly from its own file; {wait.launcherName} stays open.</p>
      <div className={'launcher-wait-status' + (running ? ' ready' : '')} aria-live="polite">
        <span className="gp-live-dot" />{expired ? 'This launch expired. Press Play again.' : running ? wait.launcherName + ' is running.' : 'Starting ' + wait.launcherName + '…'}
      </div>
      {error && <div className="form-error" role="alert">{error}</div>}
      <div className="modal-actions">
        <button type="button" className="secondary-button" onClick={cancel} disabled={busy}>Cancel</button>
        <button type="button" className="primary-button" onClick={() => void go()} disabled={busy || expired} autoFocus>Continue</button>
      </div>
    </div>
  </div>
}
