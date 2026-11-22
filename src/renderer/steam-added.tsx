import { StoreIcon } from './store-icons'
import { formatDate, formatTime } from './format'
import { addedLabel, sourceName } from '../shared/store-time'
import type { StoreAddition } from '../shared/sessions'

export const SteamTag = ({ source }: { source?: StoreAddition['source'] }) => <span className="gp-tag gp-tag-steam"><StoreIcon store={sourceName(source)} size={11} /> {sourceName(source)}</span>

/** The text block of the entry: label with the hours, dated at the last sync. */
export function SteamAddedText({ entry, showGame }: { entry: StoreAddition; showGame?: boolean }) {
  return <div>
    {showGame && <b className="sh-game">{entry.gameTitle}</b>}
    <b>{addedLabel(entry.seconds, entry.source)}</b>
    <small>{sourceName(entry.source)} sync · {formatDate(entry.syncedAt)} {formatTime(entry.syncedAt)}</small>
  </div>
}
