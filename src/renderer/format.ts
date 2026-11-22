import { useEffect, useState } from 'react'

export function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!active) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 1000)
    return () => window.clearInterval(timer)
  }, [active])
  return now
}

/** Totals: "45 min", "12.5 hrs", "1 hr". */
export function formatTotal(seconds: number): string {
  if (!seconds || seconds < 1) return '0 min'
  if (seconds < 60) return '<1 min'
  if (seconds < 3600) return Math.round(seconds / 60) + ' min'
  const hours = Math.round(seconds / 360) / 10
  return (Number.isInteger(hours) ? String(hours) : hours.toFixed(1)) + (hours === 1 ? ' hr' : ' hrs')
}

/** Session lengths: "40 sec", "45 min", "1h 12m". */
export function formatSession(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds))
  if (total < 60) return total + ' sec'
  if (total < 3600) return Math.floor(total / 60) + ' min'
  const minutes = Math.floor((total % 3600) / 60)
  return Math.floor(total / 3600) + 'h ' + minutes + 'm'
}

/** Live clock: "0:12:34", "1:02:33". */
export function formatClock(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds))
  const pad = (value: number) => String(value).padStart(2, '0')
  return Math.floor(total / 3600) + ':' + pad(Math.floor((total % 3600) / 60)) + ':' + pad(total % 60)
}

export const elapsedSeconds = (startedAt: number, now: number) => Math.max(0, Math.floor((now - startedAt) / 1000))
export const formatDate = (ms?: number) => ms ? new Date(ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' }) : 'Never'
export const formatTime = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' })
