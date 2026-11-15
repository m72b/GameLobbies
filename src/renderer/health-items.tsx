import { useState } from 'react'
import { LinkReview } from './link-review'
import type { HealthSummary, ReviewItem } from '../shared/types'

export type HealthKind = 'missing-file' | 'health-duplicate' | 'duplicate-route'
export type HealthReviewItem = Extract<ReviewItem, { kind: HealthKind }>
export const isHealthItem = (item: ReviewItem): item is HealthReviewItem => item.kind === 'missing-file' || item.kind === 'health-duplicate' || item.kind === 'duplicate-route'

export const healthLabels: Record<HealthKind, string> = { 'missing-file': 'Missing file', 'health-duplicate': 'Duplicate games', 'duplicate-route': 'Duplicate routes' }
export const healthTitle = (item: HealthReviewItem) => item.kind === 'health-duplicate' ? item.details.games.map(game => game.title).join(' / ') : item.details.title

const reasonText = { title: 'same title', 'store-id': 'same store ID', 'same-file': 'same file' } as const
type Run = (action: () => Promise<ReviewItem[]>, done: string) => void
type Props<K extends HealthKind> = { item: Extract<ReviewItem, { kind: K }>; run: Run; busy: boolean }

function MissingItem({ item, run, busy }: Props<'missing-file'>) {
  const [confirming, setConfirming] = useState(false)
  const d = item.details
  const offline = d.status === 'offline'
  const id = item.id
  return <>
    <p className="muted-text">{offline
      ? `Drive not connected${d.drive ? ' (' + d.drive + ')' : ''}. Nothing is wrong with the game; connect the drive again, or ignore this.`
      : d.folder ? 'This game’s folder is missing.' : 'This file is missing. It may have been moved or deleted.'}</p>
    <div className="root-path" title={d.path}>{d.path}</div>
    {!offline && d.candidates.length > 0 && <fieldset className="review-choices"><legend>Found in your library folders</legend>
      {d.candidates.map((candidate, index) => <div className="root-row" key={candidate}><div className="root-main"><div className="root-path" title={candidate}>{candidate}</div></div>
        <button type="button" className="primary-button" disabled={busy} onClick={() => run(() => window.lobby.health.useFound(id, index), 'Location updated')}>Use this location</button></div>)}</fieldset>}
    {confirming
      ? <div className="settings-actions"><span className="muted-text">Remove this launch route? Files on disk are not touched.</span>
        <button type="button" className="primary-button" disabled={busy} onClick={() => run(() => window.lobby.health.removeRoute(id, true), 'Route removed')}>Remove route</button>
        <button type="button" className="secondary-button" onClick={() => setConfirming(false)}>Cancel</button></div>
      : <div className="settings-actions">
        {!offline && <button type="button" className="secondary-button" disabled={busy} onClick={() => run(() => window.lobby.health.locate(id), 'Location updated')}>Locate…</button>}
        <button type="button" className="secondary-button" disabled={busy} onClick={() => setConfirming(true)}>Remove this route</button>
        <button type="button" className="secondary-button" disabled={busy} onClick={() => run(() => window.lobby.health.dismiss(id), 'Ignored')}>Ignore</button></div>}
  </>
}

function DuplicateGamesItem({ item, run, busy }: Props<'health-duplicate'>) {
  const [reviewing, setReviewing] = useState(false)
  const games = item.details.games
  const id = item.id
  return <>
    <p className="muted-text">Looks like the same game ({item.details.reasons.map(reason => reasonText[reason]).join(', ')}). Nothing is linked until you review it: linking keeps every launch route, playtime and session, and can be undone from the game page.</p>
    <ul className="review-games">{games.map(game => <li key={game.gameId}><b>{game.title}</b> · {game.store} · {game.playtimeMinutes} min{game.paths.map(path => <div className="root-path" key={path} title={path}>{path}</div>)}</li>)}</ul>
    <div className="settings-actions"><button type="button" className="secondary-button" disabled={busy} onClick={() => setReviewing(true)}>Link…</button><button type="button" className="secondary-button" disabled={busy} onClick={() => run(() => window.lobby.health.dismiss(id), 'Saved. These won’t be suggested again.')}>Not duplicates</button></div>
    {reviewing && <LinkReview gameIds={games.map(game => game.gameId)} reviewId={id} onClose={() => setReviewing(false)} onLinked={() => { setReviewing(false); run(() => window.lobby.review.list(), 'Games linked') }} />}
  </>
}
function DuplicateRouteItem({ item, run, busy }: Props<'duplicate-route'>) {
  const id = item.id
  return <>
    <p className="muted-text">This game has {item.details.routes.length} launch routes that start the same thing. Merging keeps one.</p>
    <ul className="review-games">{item.details.routes.map((route, index) => <li key={index}>{route.store}<div className="root-path" title={route.target}>{route.target}</div></li>)}</ul>
    <div className="settings-actions"><button type="button" className="primary-button" disabled={busy} onClick={() => run(() => window.lobby.health.mergeRoutes(id), 'Routes merged')}>Merge routes</button><button type="button" className="secondary-button" disabled={busy} onClick={() => run(() => window.lobby.health.dismiss(id), 'Saved. These won’t be suggested again.')}>Keep both</button></div>
  </>
}

export function HealthItem({ item, run, busy }: { item: HealthReviewItem; run: Run; busy: boolean }) {
  if (item.kind === 'missing-file') return <MissingItem item={item} run={run} busy={busy} />
  if (item.kind === 'health-duplicate') return <DuplicateGamesItem item={item} run={run} busy={busy} />
  return <DuplicateRouteItem item={item} run={run} busy={busy} />
}

export function HealthCheckCard({ say, onError }: { say(text: string): void; onError(text: string): void }) {
  const [checking, setChecking] = useState(false)
  const [summary, setSummary] = useState<HealthSummary | null>(null)
  const check = async () => {
    setChecking(true)
    try {
      setSummary(await window.lobby.health.check())
      say('Library checked')
    } catch (caught) { onError(caught instanceof Error ? caught.message : typeof caught === 'string' ? caught : 'Could not check the library') }
    finally { setChecking(false) }
  }
  return <div className="settings-card">
    <h3>Library health</h3>
    <p className="muted-text">Finds games whose files were moved or deleted, and duplicate games. Nothing on disk is ever deleted.</p>
    <div className="settings-actions"><button type="button" className="secondary-button" disabled={checking} onClick={() => void check()}>{checking ? 'Checking…' : 'Check library'}</button></div>
    {summary && <p className="muted-text">{summary.missing} missing, {summary.offline} on a drive that isn’t connected, {summary.duplicateGames} duplicate game{summary.duplicateGames === 1 ? '' : 's'}, {summary.duplicateRoutes} duplicate route{summary.duplicateRoutes === 1 ? '' : 's'}. Results are listed under Needs review.</p>}
  </div>
}
