import { useCallback, useEffect, useMemo, useState } from 'react'
import { BookOpen, Copy, Download, ExternalLink, History, Pencil, RefreshCw, Share2, Trash2, X } from 'lucide-react'
import { coverUrl } from './lobby-api'
import { sessionsApi } from './sessions-api'
import { elapsedSeconds, formatClock, formatDate, formatSession, formatTime, useNow } from './format'
import { backloggdSearchUrl, durationSeconds, editProblem, formatRange, fromLocalInput, MAX_NOTE_CHARS, MAX_SESSION_SCREENSHOTS, noteLength, sourceLabel, toLocalInput, utcOffsetMinutes, type SessionCursor, type SessionEntry, type ShareFormat } from '../shared/sessions'
import type { RunningGame, Screenshot } from '../shared/types'

const PAGE_SIZE = 50
const errorText = (error: unknown, fallback: string) => error instanceof Error && error.message ? error.message : fallback

type Mode = 'edit' | 'journal' | 'share'

/**
 * Full play history: one game's sessions (`gameId`) or all games'. Loads `PAGE_SIZE` at a time
 * from the backend ("Load more"); edit, delete, journal and share act on one session.
 */
export function SessionHistory({ gameId, title, running, onClose }: { gameId?: string; title?: string; running: RunningGame[]; onClose(): void }) {
  const [sessions, setSessions] = useState<SessionEntry[]>([])
  const [next, setNext] = useState<SessionCursor | undefined>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState('')
  const [open, setOpen] = useState<{ id: number; mode: Mode } | null>(null)
  const runningIds = useMemo(() => new Set(running.filter(item => item.state === 'running').map(item => item.gameId)), [running])
  const runKey = running.map(item => item.gameId + ':' + item.state + ':' + item.startedAt).join('|')
  const now = useNow(sessions.some(session => !session.endedAt && runningIds.has(session.gameId)))

  const load = useCallback(async (cursor: SessionCursor | null) => {
    setLoading(true); setError('')
    try {
      const page = await sessionsApi.page(gameId ?? null, cursor, PAGE_SIZE)
      setSessions(current => cursor ? [...current, ...page.sessions.filter(item => !current.some(have => have.id === item.id))] : page.sessions)
      setNext(page.next)
    } catch (e) {
      setError(errorText(e, 'Could not load the play history.'))
    } finally {
      setLoading(false)
    }
  }, [gameId])
  // A game starting or stopping changes the newest sessions: reload from the top.
  useEffect(() => { void load(null) }, [load, runKey])
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === 'Escape') onClose() }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])

  const replace = (entry: SessionEntry) => setSessions(current => current.map(item => item.id === entry.id ? entry : item).sort((a, b) => b.startedAt - a.startedAt || b.id - a.id))
  const remove = async (session: SessionEntry) => {
    const what = (gameId ? '' : session.gameTitle + ', ') + formatDate(session.startedAt) + ' ' + formatTime(session.startedAt) + ' (' + formatSession(session.seconds) + ')'
    if (!window.confirm('Delete this play session?\n\n' + what + '\n\nIts time is taken off the game\'s playtime. This can\'t be undone.')) return
    try {
      await sessionsApi.remove(session.id)
      setSessions(current => current.filter(item => item.id !== session.id))
      if (open?.id === session.id) setOpen(null)
    } catch (e) {
      setError(errorText(e, 'Could not delete the session.'))
    }
  }
  const toggle = (id: number, mode: Mode) => setOpen(current => current?.id === id && current.mode === mode ? null : { id, mode })

  return <div className="modal-scrim" onMouseDown={event => { if (event.target === event.currentTarget) onClose() }}>
    <div className="modal sh-modal" role="dialog" aria-label="Play history">
      <div className="modal-head">
        <h2><History size={16} /> Play history{title ? ' · ' + title : ' · all games'}</h2>
        <div className="sh-head-actions">
          <button type="button" title="Reload" aria-label="Reload" onClick={() => void load(null)} disabled={loading}><RefreshCw size={15} className={loading ? 'spin' : ''} /></button>
          <button type="button" aria-label="Close" onClick={onClose}><X size={17} /></button>
        </div>
      </div>
      <details className="sh-about">
        <summary>About play history</summary>
        <p>Sessions are recorded while Game Lobby is running: a game launched from Game Lobby, or (with detection on) one whose process is seen with a game window. Sessions that start and end while Game Lobby is closed aren't recorded. Games without a visible window or whose process isn't recognized can't be detected.</p>
        <p>If Game Lobby closes unexpectedly (a crash or the PC shutting down), the session is ended at the last time the game was seen running and marked <b>Interrupted</b>. Corrected sessions are marked <b>Edited</b>; editing or deleting a session changes the game's playtime by the same amount.</p>
        <p>History, notes and screenshot links are stored only on this PC, in the library database in Game Lobby's app data folder, and are included in library backups (Export library). Nothing is sent anywhere unless you copy, save or open a link.</p>
      </details>
      {error && <div className="form-error" role="alert">{error}</div>}
      <div className="sh-list">
        {!loading && sessions.length === 0 && !error && <div className="empty-inline">No play sessions recorded yet.</div>}
        {sessions.map(session => {
          const live = !session.endedAt
          const isRunning = live && runningIds.has(session.gameId)
          const mode = open?.id === session.id ? open.mode : null
          return <div key={session.id} className={'sh-row' + (live ? ' live' : '')}>
            <div className="sh-main">
              <div className="sh-when">
                {!gameId && <b className="sh-game">{session.gameTitle}</b>}
                <b>{formatDate(session.startedAt)}</b>
                <small>{formatRange(session.startedAt, live ? undefined : session.endedAt, formatTime)}</small>
              </div>
              <div className="sh-tags">
                <span className="gp-tag">{sourceLabel(session.source)}</span>
                {live && <span className="gp-tag sh-running">{isRunning ? 'Running' : 'In progress'}</span>}
                {session.interrupted && <span className="gp-tag sh-warn" title="Game Lobby closed unexpectedly; the end is the last time the game was seen running.">Interrupted</span>}
                {session.edited && <span className="gp-tag" title="Start or end corrected by hand">Edited</span>}
              </div>
              <span className="gp-dur">{isRunning ? <><span className="gp-live-dot" /> {formatClock(elapsedSeconds(session.startedAt, now))}</> : formatSession(session.seconds)}</span>
              <div className="sh-actions">
                <button type="button" title={live ? 'Running sessions can be edited after they end' : 'Correct start and end'} aria-label="Edit times" disabled={live} aria-pressed={mode === 'edit'} onClick={() => toggle(session.id, 'edit')}><Pencil size={14} /></button>
                <button type="button" title="Note and screenshots" aria-label="Journal" aria-pressed={mode === 'journal'} onClick={() => toggle(session.id, 'journal')}><BookOpen size={14} /></button>
                <button type="button" title="Share session" aria-label="Share session" aria-pressed={mode === 'share'} onClick={() => toggle(session.id, 'share')}><Share2 size={14} /></button>
                <button type="button" className="danger" title={live ? 'Running sessions can be deleted after they end' : 'Delete session'} aria-label="Delete session" disabled={live} onClick={() => void remove(session)}><Trash2 size={14} /></button>
              </div>
            </div>
            {(session.note || session.screenshots.length > 0) && mode !== 'journal' && <div className="sh-journal-view">
              {session.note && <p>{session.note}</p>}
              {session.screenshots.length > 0 && <div className="sh-shots">{session.screenshots.map(shot => <img key={shot.id} src={coverUrl(shot.src)} alt="" loading="lazy" />)}</div>}
            </div>}
            {mode === 'edit' && <EditTimes session={session} onSaved={entry => { replace(entry); setOpen(null) }} onCancel={() => setOpen(null)} />}
            {mode === 'journal' && <Journal session={session} onSaved={entry => { replace(entry); setOpen(null) }} onCancel={() => setOpen(null)} />}
            {mode === 'share' && <Share session={session} />}
          </div>
        })}
      </div>
      <div className="modal-actions">
        {next && <button type="button" className="secondary-button" disabled={loading} onClick={() => void load(next)}>{loading ? 'Loading…' : 'Load more'}</button>}
        <button type="button" className="secondary-button" onClick={onClose}>Close</button>
      </div>
    </div>
  </div>
}

function EditTimes({ session, onSaved, onCancel }: { session: SessionEntry; onSaved(entry: SessionEntry): void; onCancel(): void }) {
  const originalStart = toLocalInput(session.startedAt)
  const originalEnd = toLocalInput(session.endedAt ?? session.startedAt)
  const [start, setStart] = useState(originalStart)
  const [end, setEnd] = useState(originalEnd)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  // Unchanged fields keep their seconds; the inputs show minutes.
  const startMs = start === originalStart ? session.startedAt : fromLocalInput(start)
  const endMs = end === originalEnd ? session.endedAt ?? null : fromLocalInput(end)
  const problem = editProblem(startMs, endMs, Date.now())
  const save = async () => {
    if (problem || startMs === null || endMs === null) return setError(problem ?? '')
    setBusy(true); setError('')
    try { onSaved(await sessionsApi.update(session.id, startMs, endMs)) } catch (e) { setError(errorText(e, 'Could not save the session.')) } finally { setBusy(false) }
  }
  return <form className="sh-panel" onSubmit={event => { event.preventDefault(); void save() }}>
    <div className="sh-fields">
      <label>Start<input type="datetime-local" value={start} max={toLocalInput(Date.now())} onChange={event => setStart(event.target.value)} /></label>
      <label>End<input type="datetime-local" value={end} max={toLocalInput(Date.now())} onChange={event => setEnd(event.target.value)} /></label>
      <div className="sh-new-dur"><span>Duration</span><b>{startMs !== null && endMs !== null && endMs > startMs ? formatSession(durationSeconds(startMs, endMs)) : '–'}</b></div>
    </div>
    <p className="sh-hint">The duration becomes the time between start and end, and the game's playtime changes by the difference. Times can't overlap another session of this game or be in the future.</p>
    {(error || (problem && (start !== originalStart || end !== originalEnd))) && <div className="form-error" role="alert">{error || problem}</div>}
    <div className="sh-panel-actions">
      <button type="button" className="secondary-button" onClick={onCancel}>Cancel</button>
      <button className="primary-button" disabled={busy || !!problem || (start === originalStart && end === originalEnd)}>Save times</button>
    </div>
  </form>
}

function Journal({ session, onSaved, onCancel }: { session: SessionEntry; onSaved(entry: SessionEntry): void; onCancel(): void }) {
  const [note, setNote] = useState(session.note ?? '')
  const [chosen, setChosen] = useState<string[]>(session.screenshots.map(shot => shot.id))
  const [shots, setShots] = useState<Screenshot[] | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    let live = true
    window.lobby.screenshots.list(session.gameId).then(list => { if (live) setShots(list) }).catch(() => { if (live) setShots([]) })
    return () => { live = false }
  }, [session.gameId])
  const length = noteLength(note)
  const pick = (id: string) => setChosen(current => current.includes(id) ? current.filter(item => item !== id) : current.length >= MAX_SESSION_SCREENSHOTS ? current : [...current, id])
  const save = async () => {
    setBusy(true); setError('')
    try { onSaved(await sessionsApi.setJournal(session.id, note, chosen)) } catch (e) { setError(errorText(e, 'Could not save the note.')) } finally { setBusy(false) }
  }
  return <form className="sh-panel" onSubmit={event => { event.preventDefault(); void save() }}>
    <label className="sh-note">Note
      <textarea value={note} rows={4} placeholder="What happened this session?" onChange={event => setNote(event.target.value)} />
      <small className={length > MAX_NOTE_CHARS ? 'sh-over' : ''}>{length} / {MAX_NOTE_CHARS}</small>
    </label>
    <div className="detail-label">Screenshots ({chosen.length}/{MAX_SESSION_SCREENSHOTS})</div>
    {shots === null ? <div className="muted-text">Loading screenshots…</div>
      : shots.length === 0 ? <div className="muted-text">This game has no screenshots yet. Add them on the game page, then attach them here.</div>
        : <div className="sh-shot-picker">{shots.map(shot => <button type="button" key={shot.id} className={'sh-shot' + (chosen.includes(shot.id) ? ' chosen' : '')} aria-pressed={chosen.includes(shot.id)} title={shot.missing ? 'File missing' : undefined} onClick={() => pick(shot.id)}>
          {!shot.missing && <img src={coverUrl(shot.src)} alt="" loading="lazy" />}
        </button>)}</div>}
    {error && <div className="form-error" role="alert">{error}</div>}
    <div className="sh-panel-actions">
      <button type="button" className="secondary-button" onClick={onCancel}>Cancel</button>
      <button className="primary-button" disabled={busy || length > MAX_NOTE_CHARS}>Save journal</button>
    </div>
  </form>
}

function Share({ session }: { session: SessionEntry }) {
  const [format, setFormat] = useState<ShareFormat>('txt')
  const [text, setText] = useState('')
  const [status, setStatus] = useState('')
  const offset = utcOffsetMinutes(session.startedAt)
  useEffect(() => {
    let live = true
    setText('')
    sessionsApi.shareText(session.id, offset, format).then(value => { if (live) setText(value) }).catch(e => { if (live) setStatus(errorText(e, 'Could not build the summary.')) })
    return () => { live = false }
  }, [session.id, session.note, session.startedAt, session.endedAt, offset, format])
  const copy = async () => {
    try { await navigator.clipboard.writeText(text); setStatus('Copied to the clipboard.') } catch { setStatus('Could not copy. Select the text and copy it instead.') }
  }
  const save = async () => {
    try {
      const path = await sessionsApi.export(session.id, offset, format)
      setStatus(path ? 'Saved to ' + path : '')
    } catch (e) { setStatus(errorText(e, 'Could not save the file.')) }
  }
  return <div className="sh-panel">
    <div className="sh-share-head">
      <div className="segmented" role="group" aria-label="Summary format">
        <button type="button" className={format === 'txt' ? 'active' : ''} aria-pressed={format === 'txt'} onClick={() => setFormat('txt')}>Text</button>
        <button type="button" className={format === 'md' ? 'active' : ''} aria-pressed={format === 'md'} onClick={() => setFormat('md')}>Markdown</button>
      </div>
      <button type="button" className="secondary-button" disabled={!text} onClick={() => void copy()}><Copy size={14} /> Copy</button>
      <button type="button" className="secondary-button" disabled={!text} onClick={() => void save()}><Download size={14} /> Save as .{format}…</button>
      <a className="external-link sh-link" href={backloggdSearchUrl(session.gameTitle)} title="Opens a Backloggd search for this game in your browser">Open on Backloggd <ExternalLink size={12} /></a>
    </div>
    <textarea className="sh-preview" readOnly value={text} rows={Math.min(14, Math.max(6, text.split('\n').length + 1))} aria-label="Summary preview" />
    {status && <div className="sh-hint" role="status">{status}</div>}
  </div>
}
