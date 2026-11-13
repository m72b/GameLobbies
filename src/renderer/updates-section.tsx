import { useEffect, useState } from 'react'
import { CheckCircle2, CloudDownload, Gamepad2, RefreshCw } from 'lucide-react'
import type { UpdateInfo, UpdateProgress } from '../shared/types'
import packageInfo from '../../package.json'

type Phase = 'idle' | 'checking' | 'installing' | 'restarting'

const message = (error: unknown, fallback: string) => error instanceof Error ? error.message : fallback

function formatBytes(bytes: number) {
  return bytes >= 1024 * 1024 ? (bytes / (1024 * 1024)).toFixed(1) + ' MB' : Math.max(0, Math.round(bytes / 1024)) + ' KB'
}

function DownloadProgress({ progress }: { progress: UpdateProgress | null }) {
  const total = progress?.total
  const percent = progress && total ? Math.min(100, Math.floor(progress.downloaded / total * 100)) : null
  return <div className="scan-progress" role="status" aria-live="polite">
    <div className="scan-bar" role="progressbar" aria-label="Update download" aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent ?? undefined}>
      {percent === null ? <span className="indeterminate" /> : <span style={{ width: percent + '%' }} />}
    </div>
    <div className="scan-line"><b>Downloading update</b><span>{percent !== null ? percent + '%' : progress ? formatBytes(progress.downloaded) : 'Starting…'}</span></div>
  </div>
}

/** Settings → About and Updates: version, update check/install and the automatic-check setting. */
export function UpdatesSection({ autoCheck, onAutoCheckChange }: { autoCheck: boolean; onAutoCheckChange(enabled: boolean): void }) {
  const [phase, setPhase] = useState<Phase>('idle')
  const [info, setInfo] = useState<UpdateInfo | null>(null)
  const [error, setError] = useState('')
  const [progress, setProgress] = useState<UpdateProgress | null>(null)

  useEffect(() => {
    // An update the main window's automatic check already found.
    void window.lobby.updates.last().then(found => { if (found) setInfo(current => current ?? found) }).catch(() => undefined)
    const offProgress = window.lobby.updates.onProgress(setProgress)
    const offReady = window.lobby.updates.onReady(() => setPhase('restarting'))
    return () => { offProgress(); offReady() }
  }, [])

  const busy = phase !== 'idle'
  const check = async () => {
    setPhase('checking')
    setError('')
    try { setInfo(await window.lobby.updates.check()) }
    catch (reason) { setError(message(reason, 'Could not check for updates')) }
    finally { setPhase(current => current === 'checking' ? 'idle' : current) }
  }
  const install = async () => {
    setPhase('installing')
    setError('')
    setProgress(null)
    try { await window.lobby.updates.install() }
    catch (reason) {
      setError(message(reason, 'The update could not be installed'))
      setPhase('idle')
    }
  }

  const date = info?.date ? new Date(info.date).toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' }) : ''
  return <>
    <div className="settings-card">
      <div className="about-mark"><Gamepad2 /></div><h2>Game Lobby</h2>
      <p>Version {info?.currentVersion ?? packageInfo.version} · Local-first library manager</p>
      <p>Built with Tauri, Rust, React, TypeScript and SQLite.</p>
      <p>Free and open source under the GNU GPL v3.0. Game Lobby includes no games, ROMs, BIOS files or game artwork; it shows the games you already have.</p>
      <p>Cover art is provided by the SteamGridDB community and Steam, and stays on this PC. Game Lobby is not affiliated with or endorsed by Valve, Epic Games, GOG, Microsoft, Ubisoft, Electronic Arts, Blizzard, Amazon, itch.io, SteamGridDB or any emulator project. All names, logos and artwork belong to their owners.</p>
    </div>
    <div className="settings-card">
      <div className="card-heading">
        <div><h3>Updates</h3><p>New versions come from the Game Lobby releases page on GitHub. An update is installed only after its signature is verified, and only when you choose to.</p></div>
        <button type="button" className="secondary-button" disabled={busy} onClick={() => void check()}>
          {phase === 'checking' ? <><RefreshCw size={14} className="spin" /> Checking…</> : <><RefreshCw size={14} /> Check for updates</>}
        </button>
      </div>
      {error && <div className="form-error" role="alert">{error}</div>}
      {info && !info.available && phase !== 'checking' && <div className="update-status" role="status"><CheckCircle2 size={15} /> You're up to date.</div>}
      {info?.available && <div className="update-available">
        <div className="update-title"><b>Game Lobby {info.version} is available</b>{date && <span>Released {date}</span>}</div>
        {info.notes && <div className="update-notes" tabIndex={0} aria-label="Release notes">{info.notes}</div>}
        {phase === 'installing' && <DownloadProgress progress={progress} />}
        {phase === 'restarting' && <div className="update-status" role="status"><RefreshCw size={15} className="spin" /> Restarting…</div>}
        {phase !== 'installing' && phase !== 'restarting' && <div className="settings-actions">
          <button type="button" className="primary-button" disabled={busy} onClick={() => void install()}><CloudDownload size={14} /> Download and install</button>
        </div>}
        <small className="update-hint">Game Lobby closes while the update installs and starts again afterwards. Running play sessions are saved and your library is backed up first.</small>
      </div>}
      <div className="toggle-row"><div><b>Automatically check for updates</b><small>Check once a day, shortly after Game Lobby starts. Updates are never installed without your click.</small></div><input type="checkbox" checked={autoCheck} onChange={event => onAutoCheckChange(event.target.checked)} /></div>
    </div>
  </>
}
