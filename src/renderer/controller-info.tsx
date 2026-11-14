import { useCallback, useEffect, useRef, useState } from 'react'
import { Gamepad2 } from 'lucide-react'
import { MAX_INPUT_NOTES_CHARS, MAX_PREFERRED_CHARS } from '../shared/notes-types'
import type { ControllerSupport, GameInput } from '../shared/notes-types'
import './game-notes.css'

const none: GameInput = { support: 'unknown', supportSource: '', preferred: '', notes: '', updatedAt: 0, steamCheckedAt: 0 }
export const SUPPORT_LABELS: Record<ControllerSupport, string> = { full: 'Full controller support', partial: 'Partial controller support', none: 'Keyboard & mouse only', unknown: 'Unknown (automatic)' }
const CONTROLLERS = ['Xbox', 'PlayStation', 'Switch Pro', 'Steam Controller', 'Keyboard & mouse', 'Other']

/** Loads the stored details, then lets the backend ask Steam (rate limited, cached) for a Steam game. */
export function useGameInput(gameId: string) {
  const [input, setInput] = useState<GameInput>(none)
  const [loaded, setLoaded] = useState(false)
  const edited = useRef(false)
  useEffect(() => {
    let live = true
    edited.current = false
    setInput(none); setLoaded(false)
    window.lobby.notes.getInput(gameId).then(next => { if (live) { setInput(next); setLoaded(true) } }).catch(() => {})
    window.lobby.notes.refreshInput(gameId).then(next => { if (live && !edited.current) setInput(next) }).catch(() => {})
    return () => { live = false }
  }, [gameId])
  return { input, setInput, loaded, edited }
}

export type GameInputState = ReturnType<typeof useGameInput>

/** Small badge next to the play button when the game supports a controller (fully or partly). */
export function ControllerBadge({ input }: { input: GameInput }) {
  if (input.support !== 'full' && input.support !== 'partial') return null
  const label = input.support === 'full' ? 'Full' : 'Partial'
  return <div className={'yn-badge ' + input.support} title={SUPPORT_LABELS[input.support] + (input.supportSource === 'steam' ? ' (from Steam)' : '')}>
    <Gamepad2 size={14} /> <span>Controller: {label}</span>
  </div>
}

/** Side column block to see and change the controller details. Saved a moment after each edit. */
export function ControllerBlock({ gameId, state }: { gameId: string; state: GameInputState }) {
  const { input, setInput, loaded, edited } = state
  const [error, setError] = useState('')
  const [saved, setSaved] = useState(false)
  const timer = useRef<number | undefined>(undefined)
  const pending = useRef<{ gameId: string; value: GameInput } | null>(null)
  useEffect(() => { setError(''); setSaved(false) }, [gameId])

  const flush = useCallback(() => {
    window.clearTimeout(timer.current)
    const job = pending.current
    if (!job) return
    pending.current = null
    window.lobby.notes.saveInput(job.gameId, { support: job.value.support, preferred: job.value.preferred, notes: job.value.notes })
      .then(next => { setInput(prev => pending.current ? prev : { ...prev, supportSource: next.supportSource, support: next.support }); setError(''); setSaved(true) })
      .catch(e => { setError(e instanceof Error ? e.message : 'Could not save'); setSaved(false) })
  }, [setInput])
  // Switching games or leaving the page saves what is pending.
  useEffect(() => flush, [gameId, flush])

  const change = (patch: Partial<GameInput>) => {
    edited.current = true
    setSaved(false)
    const value = { ...input, ...patch }
    // A level picked by hand is the user's; going back to "unknown" hands it back to Steam.
    if (patch.support !== undefined) value.supportSource = patch.support === 'unknown' ? '' : 'user'
    setInput(value)
    pending.current = { gameId, value }
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(flush, patch.support !== undefined ? 0 : 800)
  }

  const source = input.supportSource === 'steam' ? 'From Steam' : input.supportSource === 'user' ? 'Set by you' : ''
  return <div className="gp-side-block"><div className="detail-label">CONTROLLER</div>
    <div className="yn-fields">
      <label>Support
        <select value={input.support} disabled={!loaded} onChange={event => change({ support: event.target.value as ControllerSupport })}>
          <option value="unknown">Unknown (automatic)</option>
          <option value="full">Full</option>
          <option value="partial">Partial</option>
          <option value="none">Keyboard &amp; mouse only</option>
        </select>
      </label>
      {source && <small className="muted-text">{source}{input.supportSource === 'user' && ' · Steam will not change it'}</small>}
      <label>Preferred controller
        <input list="yn-controllers" value={input.preferred} maxLength={MAX_PREFERRED_CHARS} disabled={!loaded} placeholder="Pick one or type your own" onChange={event => change({ preferred: event.target.value })} />
        <datalist id="yn-controllers">{CONTROLLERS.map(name => <option key={name} value={name} />)}</datalist>
      </label>
      <label>Input notes
        <textarea value={input.notes} rows={3} maxLength={MAX_INPUT_NOTES_CHARS} disabled={!loaded} placeholder="e.g. use Steam Input layout X" onChange={event => change({ notes: event.target.value })} />
      </label>
      <small className="muted-text" role="status">{saved ? 'Saved' : ''}</small>
      {error && <div className="form-error" role="alert">{error}</div>}
    </div>
  </div>
}
