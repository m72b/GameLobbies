import { StoreIcon } from './store-icons'
import { formatDate, formatTime } from './format'
import { addedLabel, addedTooltip } from '../shared/store-time'
import type { StoreAddition } from '../shared/sessions'

/** The "Steam" pill of the read-only "Added from Steam" activity entry. */
export const SteamTag = () => <span className="gp-tag gp-tag-steam"><StoreIcon store="Steam" size={11} /> Steam</span>

/** The text block of the entry: label with the hours, dated at the last sync. */
export function SteamAddedText({ entry, showGame }: { entry: StoreAddition; showGame?: boolean }) {
  return <div>
    {showGame && <b className="sh-game">{entry.gameTitle}</b>}
    <b>{addedLabel(entry.seconds)}</b>
    <small>Steam sync · {formatDate(entry.syncedAt)} {formatTime(entry.syncedAt)}</small>
  </div>
}


