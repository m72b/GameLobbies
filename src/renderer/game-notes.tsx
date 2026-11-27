import { useCallback, useEffect, useRef, useState } from 'react'
import { Pencil, Star, Trash2 } from 'lucide-react'
import { coverUrl } from './lobby-api'
import { MAX_MEMORY_CHARS, MAX_NOTES_CHARS } from '../shared/notes-types'
import type { GameMemory, GameNotes } from '../shared/notes-types'
import type { Screenshot } from '../shared/types'
import './game-notes.css'

const empty: GameNotes = { rating: null, notes: '', updatedAt: 0, memories: [] }
const errorText = (error: unknown) => error instanceof Error ? error.message : 'Could not save'

export function todayIso() {
  const d = new Date()
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

function formatMemoryDate(iso: string) {
  const [y, m, d] = iso.split('-').map(Number)
  return new Date(y, m - 1, d).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
}

export function useGameNotes(gameId: string) {
  const [data, setData] = useState<GameNotes>(empty)
  const [loaded, setLoaded] = useState(false)
  useEffect(() => {
    let live = true
    setData(empty); setLoaded(false)
    window.lobby.notes.get(gameId).then(next => { if (live) { setData(next); setLoaded(true) } }).catch(() => {})
    return () => { live = false }
  }, [gameId])
  return { data, setData, loaded }
}

/** Five stars in half steps. Each half of a star is its own button; clicking the current value clears it. */
function StarPicker({ value, onChange, disabled }: { value: number | null; onChange(next: number | null): void; disabled?: boolean }) {
  const [hover, setHover] = useState<number | null>(null)
  const shown = hover ?? value ?? 0
  return <div className="yn-stars" role="group" aria-label="Your score" onMouseLeave={() => setHover(null)}>
    {[1, 2, 3, 4, 5].map(star => {
      const fill = Math.max(0, Math.min(1, shown - (star - 1)))
      return <span className="yn-star" key={star}>
        <Star size={22} className="yn-star-bg" />
        <span className="yn-star-fill" style={{ width: fill * 100 + '%' }}><Star size={22} fill="currentColor" /></span>
        {[star - 0.5, star].map((half, i) => <button key={half} type="button" disabled={disabled}
          className={'yn-star-hit ' + (i === 0 ? 'left' : 'right')}
          aria-label={half + ' stars'} aria-pressed={value === half}
          onMouseEnter={() => setHover(half)} onFocus={() => setHover(half)} onBlur={() => setHover(null)}
          onClick={() => onChange(value === half ? null : half)} />)}
      </span>
    })}
  </div>
}

/** Side column block: the user's own score, kept apart from the IGDB ratings. */
export function YourScore({ gameId }: { gameId: string }) {
  const { data, setData, loaded } = useGameNotes(gameId)
  const [error, setError] = useState('')
  useEffect(() => setError(''), [gameId])
  const change = (rating: number | null) => {
    setError('')
    window.lobby.notes.setRating(gameId, rating).then(next => setData(prev => ({ ...prev, rating: next.rating }))).catch(e => setError(errorText(e)))
  }
  return <div className="gp-side-block"><div className="detail-label">Your score</div>
    <StarPicker value={data.rating} onChange={change} disabled={!loaded} />
    <div className="yn-score-line">
      <span className="muted-text">{data.rating === null ? 'Not rated' : <b className="yn-score">{data.rating}</b>}{data.rating !== null && ' out of 5'}</span>
      {data.rating !== null && <button className="yn-link" onClick={() => change(null)}>Clear</button>}
    </div>
    {error && <div className="form-error" role="alert">{error}</div>}
  </div>
}

type SaveState = 'idle' | 'saving' | 'saved' | 'error'

export function GameNotesSection({ gameId, shots }: { gameId: string; shots: Screenshot[] }) {
  const { data, setData, loaded } = useGameNotes(gameId)
  const [text, setText] = useState('')
  const [state, setState] = useState<SaveState>('idle')
  const [error, setError] = useState('')
  const dirty = useRef(false)
  const timer = useRef<number | undefined>(undefined)
  const latest = useRef({ gameId, text })
  latest.current = { gameId, text }

  const save = useCallback((id: string, value: string) => {
    window.clearTimeout(timer.current)
    dirty.current = false
    setState('saving')
    window.lobby.notes.setText(id, value)
      .then(() => { if (latest.current.gameId === id && !dirty.current) { setState('saved'); setError('') } })
      .catch(e => { if (latest.current.gameId === id) { setState('error'); setError(errorText(e)) } })
  }, [])

  useEffect(() => { if (loaded) { setText(data.notes); dirty.current = false; setState('idle'); setError('') } }, [gameId, loaded]) // eslint-disable-line react-hooks/exhaustive-deps
  // Leaving the game (or the page) saves what is pending.
  useEffect(() => () => {
    window.clearTimeout(timer.current)
    if (dirty.current) void window.lobby.notes.setText(latest.current.gameId, latest.current.text).catch(() => {})
  }, [gameId])

  const edit = (value: string) => {
    setText(value)
    dirty.current = true
    setState('idle')
    window.clearTimeout(timer.current)
    timer.current = window.setTimeout(() => save(gameId, value), 800)
  }

  return <section className="yn-section">
    <h2>Notes</h2>
    <textarea className="yn-notes" value={text} maxLength={MAX_NOTES_CHARS} rows={5} disabled={!loaded}
      placeholder="Quick notes about this game, only for you" aria-label="Notes"
      onChange={event => edit(event.target.value)} onBlur={() => { if (dirty.current) save(gameId, text) }} />
    <div className="yn-hint">
      <span role="status">{state === 'saving' ? 'Saving…' : state === 'saved' ? 'Saved' : ''}</span>
      <span>{text.length} / {MAX_NOTES_CHARS}</span>
    </div>
    {error && <div className="form-error" role="alert">{error}</div>}
    <Memories gameId={gameId} memories={data.memories} shots={shots} onData={setData} />
  </section>
}

function Memories({ gameId, memories, shots, onData }: { gameId: string; memories: GameMemory[]; shots: Screenshot[]; onData(next: GameNotes): void }) {
  const [editing, setEditing] = useState<number | 'new' | null>(null)
  const [date, setDate] = useState(todayIso())
  const [text, setText] = useState('')
  const [shotId, setShotId] = useState('')
  const [error, setError] = useState('')
  useEffect(() => { setEditing(null); setError('') }, [gameId])

  const open = (memory?: GameMemory) => {
    setEditing(memory ? memory.id : 'new')
    setDate(memory?.date ?? todayIso()); setText(memory?.text ?? ''); setShotId(memory?.screenshotId ?? ''); setError('')
  }
  const submit = () => {
    if (!text.trim()) { setError('Write something first.'); return }
    window.lobby.notes.saveMemory(gameId, { id: editing === 'new' || editing === null ? undefined : editing, date, text, screenshotId: shotId || null })
      .then(next => { onData(next); setEditing(null); setError('') })
      .catch(e => setError(errorText(e)))
  }
  const remove = (memory: GameMemory) => {
    window.lobby.notes.deleteMemory(gameId, memory.id).then(onData).catch(e => setError(errorText(e)))
  }
  const shotLabel = (shot: Screenshot, index: number) => `Screenshot ${index + 1} · ${new Date(shot.addedAt).toLocaleDateString()}`

  return <div className="yn-memories">
    <div className="yn-memories-head">
      <h3>Memories</h3>
      {editing === null && <button className="secondary-button yn-add" onClick={() => open()}>Add memory</button>}
    </div>
    {editing !== null && <form className="yn-form" onSubmit={event => { event.preventDefault(); submit() }}>
      <label>Date<input type="date" value={date} min="1970-01-01" max="2200-12-31" required onChange={event => setDate(event.target.value)} /></label>
      <label>What happened<textarea value={text} maxLength={MAX_MEMORY_CHARS} rows={3} autoFocus onChange={event => setText(event.target.value)} /></label>
      {shots.length > 0 && <label>Screenshot
        <select value={shotId} onChange={event => setShotId(event.target.value)}>
          <option value="">None</option>
          {shots.map((shot, i) => <option key={shot.id} value={shot.id}>{shotLabel(shot, i)}</option>)}
        </select>
      </label>}
      {error && <div className="form-error" role="alert">{error}</div>}
      <div className="yn-form-actions">
        <button type="button" className="secondary-button" onClick={() => setEditing(null)}>Cancel</button>
        <button type="submit" className="primary-button">{editing === 'new' ? 'Add' : 'Save'}</button>
      </div>
    </form>}
    {editing === null && error && <div className="form-error" role="alert">{error}</div>}
    {memories.length === 0 && editing === null && <div className="gp-empty">No memories yet. Jot down a moment worth remembering.</div>}
    <ul className="yn-list">
      {memories.map(memory => {
        const shot = memory.screenshotId ? shots.find(item => item.id === memory.screenshotId) : undefined
        return <li key={memory.id}>
          {shot && !shot.missing && <img className="yn-thumb" src={coverUrl(shot.src)} alt="Linked screenshot" loading="lazy" />}
          <div className="yn-body"><b>{formatMemoryDate(memory.date)}</b><p>{memory.text}</p></div>
          <div className="yn-row-actions">
            <button className="gp-icon" title="Edit memory" aria-label="Edit memory" onClick={() => open(memory)}><Pencil size={16} /></button>
            <button className="gp-icon" title="Delete memory" aria-label="Delete memory" onClick={() => remove(memory)}><Trash2 size={16} /></button>
          </div>
        </li>
      })}
    </ul>
  </div>
}
