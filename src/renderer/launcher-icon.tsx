import { useEffect, useState } from 'react'
import { Bot, Car, Castle, Code, Crown, Dices, Flame, Folder, Gamepad2, Ghost, Heart, Joystick, Package, Puzzle, Rocket, Shield, Skull, Star, Sword, Swords, Terminal, Trophy, Wrench, Zap, type LucideIcon } from 'lucide-react'
import type { CustomLauncher, GameRoute } from '../shared/types'
import { launcherOfRoute, type LauncherIconName } from '../shared/launchers'
import { coverUrl } from './lobby-api'
import { StoreIcon } from './store-icons'

/** The built-in launcher icons; the names match `LAUNCHER_ICONS` (and Rust's `BUILTIN_ICONS`). */
export const launcherIcons: Record<LauncherIconName, LucideIcon> = {
  'gamepad-2': Gamepad2, joystick: Joystick, rocket: Rocket, swords: Swords, sword: Sword, shield: Shield, crown: Crown, castle: Castle,
  ghost: Ghost, skull: Skull, bot: Bot, puzzle: Puzzle, dices: Dices, trophy: Trophy, star: Star, heart: Heart,
  flame: Flame, zap: Zap, car: Car, terminal: Terminal, code: Code, wrench: Wrench, package: Package, folder: Folder
}

type IconLook = Pick<CustomLauncher, 'iconKind' | 'iconValue' | 'color' | 'name'>

/** A launcher's icon in its own colour: a built-in icon, or its image (falls back to the gamepad). */
export function LauncherIcon({ launcher, size = 12, className }: { launcher: IconLook; size?: number; className?: string }) {
  const [failed, setFailed] = useState(false)
  const classes = 'store-icon launcher-icon ' + (className ?? '')
  if (launcher.iconKind === 'image' && !failed) {
    return <img className={classes + ' launcher-icon-image'} src={coverUrl('lobby-cover://' + launcher.iconValue)} width={size} height={size} alt="" aria-hidden="true" onError={() => setFailed(true)} />
  }
  const Icon = launcherIcons[launcher.iconValue as LauncherIconName] ?? Gamepad2
  return <Icon className={classes} size={size} color={launcher.color} aria-hidden="true" />
}

/** A route's icon: its launcher's for routes run by or labelled with a launcher, the store's otherwise. */
export function RouteIcon({ route, launchers, size = 12, className }: { route: Pick<GameRoute, 'sourceId' | 'store' | 'launcherId'>; launchers: readonly CustomLauncher[]; size?: number; className?: string }) {
  const launcher = launcherOfRoute(route, launchers)
  return launcher ? <LauncherIcon launcher={launcher} size={size} className={className} /> : <StoreIcon store={route.store} size={size} className={className} />
}

/** The custom launchers, kept current (`launchers:changed`). */
export function useLaunchers(): CustomLauncher[] {
  const [launchers, setLaunchers] = useState<CustomLauncher[]>([])
  useEffect(() => {
    let live = true
    const load = () => window.lobby.launchers.list().then(next => { if (live) setLaunchers(next) }).catch(() => {})
    void load()
    const off = window.lobby.launchers.onChanged(() => void load())
    return () => { live = false; off() }
  }, [])
  return launchers
}
