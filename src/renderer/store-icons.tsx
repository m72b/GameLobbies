import { siBattledotnet, siEa, siEpicgames, siGogdotcom, siItchdotio, siSteam, siUbisoft } from 'simple-icons'
import { FolderOpen, Gamepad2 } from 'lucide-react'

// Brand icons come from simple-icons (CC0). Stores without a brand icon in the
// installed version (Xbox, Amazon Games) fall back to a short text mark.
const brandPaths: Record<string, string> = {
  Steam: siSteam.path,
  'Epic Games': siEpicgames.path,
  GOG: siGogdotcom.path,
  'EA app': siEa.path,
  'Battle.net': siBattledotnet.path,
  Ubisoft: siUbisoft.path,
  'itch.io': siItchdotio.path
}

const textMarks: Record<string, string> = { Xbox: 'X', 'Amazon Games': 'a', Humble: 'H' }

const displayNames: Record<string, string> = {
  'Xbox / Microsoft Store': 'Xbox',
  'Ubisoft Connect': 'Ubisoft'
}

export function normalizeStoreName(name: string): string {
  return displayNames[name] ?? name
}

export function StoreIcon({ store, size = 12, className }: { store: string; size?: number; className?: string }) {
  const name = normalizeStoreName(store)
  const path = brandPaths[name]
  if (path) {
    return (
      <svg className={'store-icon ' + (className ?? '')} viewBox="0 0 24 24" width={size} height={size} fill="currentColor" aria-hidden="true" focusable="false">
        <path d={path} />
      </svg>
    )
  }
  if (name === 'Emulator') return <Gamepad2 className={'store-icon ' + (className ?? '')} size={size} aria-hidden="true" />
  if (name === 'Custom') return <FolderOpen className={'store-icon ' + (className ?? '')} size={size} aria-hidden="true" />
  const text = textMarks[name] ?? (name ? name[0].toUpperCase() : '•')
  return <span className={'store-icon store-icon-text ' + (className ?? '')} style={{ fontSize: Math.round(size * 0.9) }} aria-hidden="true">{text}</span>
}
