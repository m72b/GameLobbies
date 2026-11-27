import { useEffect, useRef, useState, type ReactNode } from 'react'
import { X } from 'lucide-react'
import { currentMotion, durationFor, play } from './motion'

const FOCUSABLE = 'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),summary,[tabindex]:not([tabindex="-1"])'
const ENTER_MS = 220
const EXIT_MS = 160

/**
 * A panel that slides in from the right over a dimmed scrim. Focus stays inside it (Tab cycles,
 * Escape and the scrim close it) and returns to where it was when it closes. Render it while
 * `open`; it keeps itself mounted for the slide-out and then calls `onClosed`.
 */
export function Sheet({ open, title, subtitle, onClose, children }: { open: boolean; title: string; subtitle?: string; onClose(): void; children: ReactNode }) {
  const panel = useRef<HTMLDivElement>(null)
  const scrim = useRef<HTMLDivElement>(null)
  const [shown, setShown] = useState(open)
  const restore = useRef<HTMLElement | null>(null)
  const closing = useRef(false)

  useEffect(() => {
    if (open) { setShown(true); closing.current = false }
  }, [open])

  // Slide in.
  useEffect(() => {
    if (!open || !shown) return
    restore.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    const level = currentMotion()
    const ms = durationFor(ENTER_MS, level)
    if (panel.current) play(panel.current, level === 'full' ? [{ transform: 'translateX(28px)', opacity: 0 }, { transform: 'none', opacity: 1 }] : [{ opacity: 0 }, { opacity: 1 }], ms)
    if (scrim.current) play(scrim.current, [{ opacity: 0 }, { opacity: 1 }], ms)
    const first = panel.current?.querySelector<HTMLElement>('[data-autofocus]') ?? panel.current?.querySelector<HTMLElement>(FOCUSABLE)
    first?.focus({ preventScroll: true })
    // Focus goes back to the control that opened the sheet.
    return () => { const back = restore.current; restore.current = null; if (back?.isConnected) back.focus({ preventScroll: true }) }
    // Only on first show: re-running would steal focus from a field being typed in.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shown])

  // Slide out, then unmount.
  useEffect(() => {
    if (open || !shown || closing.current) return
    closing.current = true
    const level = currentMotion()
    const ms = durationFor(EXIT_MS, level)
    const done = () => setShown(false)
    const slide = panel.current && play(panel.current, level === 'full' ? [{ transform: 'none', opacity: 1 }, { transform: 'translateX(20px)', opacity: 0 }] : [{ opacity: 1 }, { opacity: 0 }], ms, { fill: 'forwards' })
    if (scrim.current) play(scrim.current, [{ opacity: 1 }, { opacity: 0 }], ms, { fill: 'forwards' })
    if (slide) slide.finished.then(done, done)
    else done()
  }, [open, shown])

  // Escape closes; Tab stays inside. Capture phase so the app's own Escape (leave Settings) does not also fire.
  useEffect(() => {
    if (!open) return
    const key = (event: KeyboardEvent) => {
      if (event.key === 'Escape') { event.preventDefault(); event.stopImmediatePropagation(); onClose(); return }
      if (event.key !== 'Tab' || !panel.current) return
      const items = [...panel.current.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(item => item.offsetParent !== null || item === document.activeElement)
      if (!items.length) { event.preventDefault(); return }
      const active = document.activeElement
      const first = items[0]; const last = items[items.length - 1]
      if (!panel.current.contains(active)) { event.preventDefault(); first.focus(); return }
      if (event.shiftKey && active === first) { event.preventDefault(); last.focus() }
      else if (!event.shiftKey && active === last) { event.preventDefault(); first.focus() }
    }
    window.addEventListener('keydown', key, true)
    return () => window.removeEventListener('keydown', key, true)
  }, [open, onClose])

  if (!shown) return null
  return <div className="sheet-root">
    <div className="sheet-scrim" ref={scrim} onMouseDown={onClose} />
    <div className="sheet" ref={panel} role="dialog" aria-modal="true" aria-label={title}>
      <header className="sheet-head">
        <div><h2>{title}</h2>{subtitle && <p>{subtitle}</p>}</div>
        <button type="button" className="sheet-close" aria-label="Close" onClick={onClose}><X size={16} /></button>
      </header>
      <div className="sheet-body">{children}</div>
    </div>
  </div>
}
