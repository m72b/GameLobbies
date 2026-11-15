import { useEffect, useMemo, useState } from 'react'
import { Plus, X } from 'lucide-react'
import { LauncherIcon } from './launcher-icon'
import { assignTargetLabel, isLabelOnly } from '../shared/launchers'
import type { AssignmentOption, CustomLauncher, Game, LauncherTargetKind } from '../shared/types'

/**
 * The game page's LAUNCHERS block: the custom launchers this game shows under, and a picker to add
 * it to another one. Only ids go to Rust (game, launcher, which of the game's routes and kinds);
 * the target is always taken from the game's own route, and a new launch route is confirmed in a
 * native dialog. Built-in stores can't take a game from elsewhere; ROMs choose their emulator in
 * the emulator picker above.
 */
export function LauncherBlock({ game, launchers, libraryVersion, onAssign, onUnassign }: {
  game: Game
  launchers: CustomLauncher[]
  libraryVersion: number
  onAssign(launcherId: string, routeIndex?: number, kind?: LauncherTargetKind): Promise<void>
  onUnassign(launcherId: string): Promise<void>
}) {
  const [options, setOptions] = useState<AssignmentOption[]>([])
  const [choice, setChoice] = useState('')
  const [target, setTarget] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const routesKey = game.routes.map(route => route.sourceId + '|' + route.launchTarget + '|' + (route.launcherId ?? '') + '|' + (route.assigned ? 1 : 0)).join('\n')
  useEffect(() => {
    let live = true
    window.lobby.launchers.assignments(game.id).then(next => { if (live) setOptions(next) }).catch(() => { if (live) setOptions([]) })
    return () => { live = false }
  }, [game.id, routesKey, libraryVersion, launchers])
  useEffect(() => { setChoice(''); setTarget(0); setError('') }, [game.id])

  const byId = useMemo(() => new Map(launchers.map(launcher => [launcher.id, launcher])), [launchers])
  const listed = options.filter(option => option.listed && byId.has(option.launcherId))
  const available = options.filter(option => !option.listed && byId.has(option.launcherId))
  const chosen = available.find(option => option.launcherId === choice)
  const chosenLauncher = chosen ? byId.get(chosen.launcherId) : undefined
  const targets = chosen?.targets ?? []

  const run = async (work: () => Promise<void>) => {
    setBusy(true); setError('')
    try { await work() } catch (e) { setError(e instanceof Error ? e.message : 'Something went wrong') } finally { setBusy(false) }
  }
  const add = () => run(async () => {
    if (!chosen || !chosenLauncher) return
    const picked = targets[target]
    await onAssign(chosen.launcherId, picked?.routeIndex, picked?.kind)
    setChoice(''); setTarget(0)
  })

  if (launchers.length === 0) return null
  return <div className="gp-side-block gp-launchers"><div className="detail-label">LAUNCHERS</div>
    {listed.length === 0 && <div className="muted-text">Not in any custom launcher.</div>}
    {listed.map(option => {
      const launcher = byId.get(option.launcherId)!
      return <div className="gp-route-row" key={option.launcherId}>
        <span className="store-badge small"><LauncherIcon launcher={launcher} /></span>
        <span className="route-name"><span className="route-label">{launcher.name}</span><small>{option.assigned ? (isLabelOnly(launcher) ? 'Listed here' : 'Plays through ' + launcher.name) : 'Added in the launcher'}</small></span>
        {option.assigned && <button type="button" className="gp-launcher-remove" disabled={busy} title={'Remove from ' + launcher.name} aria-label={'Remove from ' + launcher.name} onClick={() => void run(() => onUnassign(option.launcherId))}><X size={13} /></button>}
      </div>
    })}
    {available.length > 0 && <div className="gp-launcher-add">
      <select aria-label="Add to a launcher" value={choice} disabled={busy} onChange={event => { setChoice(event.target.value); setTarget(0); setError('') }}>
        <option value="">Add to a launcher…</option>
        {available.map(option => <option key={option.launcherId} value={option.launcherId}>{byId.get(option.launcherId)!.name}{option.problem ? ' (can’t take this game)' : ''}</option>)}
      </select>
      {chosen && !chosen.problem && targets.length > 1 && <select aria-label={'What ' + chosenLauncher?.name + ' gets'} value={target} disabled={busy} onChange={event => setTarget(Number(event.target.value))}>
        {targets.map((item, i) => <option key={item.kind + item.target} value={i} title={item.target}>{assignTargetLabel(item, game.routes)}</option>)}
      </select>}
      {chosen && !chosen.problem && targets.length === 1 && <div className="muted-text gp-path" title={targets[0].target}>{assignTargetLabel(targets[0], game.routes)}</div>}
      {chosen?.problem && <div className="inline-warning">{chosen.problem}</div>}
      {chosen && !chosen.problem && <button type="button" className="secondary-button" disabled={busy} onClick={() => void add()}><Plus size={14} /> {chosenLauncher && isLabelOnly(chosenLauncher) ? 'List under ' + chosenLauncher.name : 'Play with ' + chosenLauncher?.name}</button>}
    </div>}
    {error && <div className="form-error" role="alert">{error}</div>}
  </div>
}

/** A launch waiting for its custom launcher (`library.launch` returned `waiting`). */
export interface WaitingLaunch { game: Game; pendingId: string; launcherName: string }

/**
 * "Waiting for <launcher>": the launcher's program was started on its own. Continue confirms the
 * pending launch (Rust launches what it checked when Play was pressed); the renderer sends only the
 * game and pending ids. The launcher's process is watched to show when it is open.
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
      <p>Continue when it’s open and signed in. Game Lobby then starts <b>{wait.game.title}</b> through it.</p>
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
