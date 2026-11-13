/** Grid spans for the game page's picture wall (a dense CSS grid of 16:9 cells). */
export interface Span { cols: number; rows: number }

/** Columns for a wall `width` pixels wide: cells of roughly 150-220 px, at least 2. */
export function wallColumns(width: number): number {
  if (!Number.isFinite(width) || width <= 0) return 2
  return Math.max(2, Math.min(8, Math.round(width / 180)))
}

/**
 * The first picture is the feature: 2x2 cells (the full width when there are only 2 columns).
 * The others follow their aspect ratio (width / height): panoramas take 2 columns, tall
 * pictures 2 rows, everything else 1 cell. Unknown or invalid ratios count as 16:9.
 */
export function wallSpans(aspects: Array<number | undefined>, columns: number): Span[] {
  const cols = Math.max(1, Math.floor(columns))
  return aspects.map((raw, index) => {
    const aspect = raw && Number.isFinite(raw) && raw > 0 ? raw : 16 / 9
    if (index === 0) return { cols: Math.min(2, cols), rows: 2 }
    if (aspect >= 2.4 && cols >= 3) return { cols: 2, rows: 1 }
    if (aspect <= 0.85) return { cols: 1, rows: 2 }
    return { cols: 1, rows: 1 }
  })
}
