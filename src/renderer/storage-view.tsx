import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { HardDrive, RefreshCw } from 'lucide-react'
import type { StorageEntry, StorageProgress, StorageReport } from '../shared/types'
import { formatBytes, sortStorage, type StorageSort } from '../shared/storage'
import { formatDate } from './format'

const SORT_KEY = 'gameLobby.storageSort'
const sorts: StorageSort[] = ['size', 'name', 'lastPlayed']
function storedSort(): StorageSort {
  try {
    const value = window.localStorage.getItem(SORT_KEY)
    return sorts.includes(value as StorageSort) ? value as StorageSort : 'size'
  } catch { return 'size' }
}

/** Installed games with their size on disk. Sizes come from the backend's cache; "Rescan sizes" measures again. */
export function StorageView({ onOpen, onClose, notice }: { onOpen: (gameId: string) => void; onClose: () => void; notice: (message: string) => void }) {
  const [report, setReport] = useState<StorageReport | null>(null)
  const [sortBy, setSortByState] = useState<StorageSort>(storedSort)
  const [progress, setProgress] = useState<StorageProgress | null>(null)
  const [scanning, setScanning] = useState(false)
  const noticeRef = useRef(notice)
  noticeRef.current = notice
  const setSortBy = (value: StorageSort) => { setSortByState(value); try { window.localStorage.setItem(SORT_KEY, value) } catch { /* not persisted */ } }

  const load = useCallback(() => window.lobby.storage.list().then(setReport).catch(error => noticeRef.current(error instanceof Error ? error.message : 'Could not read sizes')), [])
  useEffect(() => {
    void load()
    const off = window.lobby.storage.onProgress(setProgress)
    const offLibrary = window.lobby.library.onChanged(() => void load())
    return () => { off(); offLibrary() }
  }, [load])

  const rescan = async () => {
    setScanning(true)
    try { setReport(await window.lobby.storage.rescan()) }
    catch (error) { notice(error instanceof Error ? error.message : 'Size scan failed') }
    finally { setScanning(false); setProgress(null) }
  }
  const uninstall = async (entry: StorageEntry) => {
    if (!entry.uninstall) return  // the backend asks for confirmation in a native dialog
    try { await window.lobby.storage.uninstall(entry.gameId, entry.uninstall.routeIndex) }
    catch (error) { notice(error instanceof Error ? error.message : 'Could not open the launcher') }
  }
  const entries = useMemo(() => sortStorage(report?.entries ?? [], sortBy), [report, sortBy])
  const unknown = entries.filter(entry => entry.bytes === undefined).length

  return <div className="storage-view">
    <div className="toolbar">
      <h1>Storage</h1>
      <div className="toolbar-actions">
        <button className="secondary-button" onClick={onClose}>Back to library</button>
        <button className="primary-button" onClick={() => void rescan()} disabled={scanning}><RefreshCw size={16} className={scanning ? 'spin' : ''} /> {scanning ? 'Scanning…' : 'Rescan sizes'}</button>
      </div>
    </div>
    <div className="library-meta">
      <span className="meta-count">{entries.length} installed</span>
      <span className="storage-total"><HardDrive size={16} /> {formatBytes(report?.totalBytes ?? 0)} total{unknown ? ' · ' + unknown + ' unknown' : ''}</span>
      <span className="meta-spacer" />
      <label className="sort-control">Sort<select value={sortBy} onChange={event => setSortBy(event.target.value as StorageSort)}><option value="size">Size</option><option value="name">Name</option><option value="lastPlayed">Last played</option></select></label>
    </div>
    {scanning && progress && <div className="storage-progress" role="status"><div className="storage-bar"><span style={{ width: (progress.total ? Math.round(progress.done / progress.total * 100) : 0) + '%' }} /></div><small>{progress.done} of {progress.total}{progress.title ? ' · ' + progress.title : ''}</small></div>}
    <div className="storage-list">
      {report && !entries.length && <div className="empty"><HardDrive size={30} /><h3>No installed games</h3><p>Games found by a scan as installed show up here</p></div>}
      {entries.map(entry => <div className="storage-row" key={entry.gameId}>
        <button className="storage-title" onClick={() => onOpen(entry.gameId)} title={entry.installPath}>
          <strong>{entry.title}</strong>
          <small>{entry.store}{entry.installPath ? ' · ' + entry.installPath : ' · no install folder known'}{entry.lastPlayed ? ' · played ' + formatDate(entry.lastPlayed) : ''}</small>
        </button>
        <div className="storage-size" title={entry.measuredAt ? 'Measured ' + formatDate(entry.measuredAt) + (entry.source === 'manifest' ? ' (from the Steam manifest)' : '') : undefined}>
          {entry.bytes === undefined ? <span className="muted-text">{entry.installPath ? 'Not measured' : 'Unknown'}</span> : <>{entry.partial ? '≥ ' : ''}{formatBytes(entry.bytes)}</>}
        </div>
        {entry.uninstall ? <button className="secondary-button" onClick={() => void uninstall(entry)}>Uninstall…</button> : <span />}
      </div>)}
    </div>
  </div>
}
