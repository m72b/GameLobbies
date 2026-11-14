/**
 * Layout of the game page's picture wall: a grid of 16:9 cells where each picture takes a
 * 2x2, 2x1, 1x2 or 1x1 block. Sizes look random but are stable: they come from a hash of the
 * picture's key, so the wall doesn't reshuffle between renders or restarts. After packing, gaps
 * are filled by growing neighbouring tiles, so the wall ends as a clean block (at most a part
 * row at the bottom, filled from the left).
 */
export interface Span { cols: number; rows: number }
export interface WallItem { key: string; aspect?: number }
/** A placed tile: 1-based grid lines, as CSS `grid-column` / `grid-row` start values. */
export interface Placement extends Span { col: number; row: number }

/** Columns for a wall `width` pixels wide: 3 when narrow, 4 otherwise. */
export function wallColumns(width: number): number {
  if (!Number.isFinite(width) || width < 560) return 3
  return 4
}

/** A well-mixed 32-bit hash of a string (FNV-1a, then a murmur3 finaliser). */
export function hashKey(key: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < key.length; i++) {
    h ^= key.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  h ^= h >>> 16
  h = Math.imul(h, 0x85ebca6b)
  h ^= h >>> 13
  h = Math.imul(h, 0xc2b2ae35)
  h ^= h >>> 16
  return h >>> 0
}

const BIG: Span = { cols: 2, rows: 2 }
const WIDE: Span = { cols: 2, rows: 1 }
const TALL: Span = { cols: 1, rows: 2 }
const SMALL: Span = { cols: 1, rows: 1 }

/** Wider than 16:9 (with a little slack) never gets the tall shape. */
const TALL_MAX_ASPECT = 16 / 9 + 0.01
/** Narrower than this (portrait) prefers the tall shape. */
const PORTRAIT_ASPECT = 0.9

/**
 * Sizes for the wall in order. The first picture is the 2x2 feature. The others are drawn from
 * their key's hash: about 15% 2x2, 25% 2x1, 8% 1x2 and 52% 1x1. Pictures wider than 16:9 never
 * get 1x2, portrait ones get 1x2 instead of 2x1 or 1x1, and a 2x2 never follows another 2x2.
 */
export function wallSpans(items: WallItem[], columns: number): Span[] {
  const cols = Math.max(1, Math.floor(columns))
  let previousBig = false
  return items.map((item, index) => {
    const aspect = item.aspect && Number.isFinite(item.aspect) && item.aspect > 0 ? item.aspect : undefined
    let span: Span
    if (index === 0) span = BIG
    else {
      const roll = hashKey(item.key) % 100
      span = roll < 15 ? BIG : roll < 40 ? WIDE : roll < 48 ? TALL : SMALL
      if (aspect !== undefined && aspect > TALL_MAX_ASPECT && span === TALL) span = WIDE
      if (aspect !== undefined && aspect < PORTRAIT_ASPECT && (span === WIDE || span === SMALL)) span = TALL
      if (span === BIG && previousBig) span = aspect !== undefined && aspect < PORTRAIT_ASPECT ? TALL : WIDE
    }
    previousBig = span === BIG
    return { cols: Math.min(span.cols, cols), rows: span.rows }
  })
}

/**
 * Places the tiles like CSS `grid-auto-flow: dense`: each goes into the first free spot that
 * fits, scanning rows from the top. Returns 1-based grid lines.
 */
export function placeTiles(spans: Span[], columns: number): Placement[] {
  const cols = Math.max(1, Math.floor(columns))
  const taken: boolean[][] = []
  const free = (row: number, col: number, span: Span) => {
    for (let r = row; r < row + span.rows; r++) {
      for (let c = col; c < col + span.cols; c++) if (taken[r]?.[c]) return false
    }
    return true
  }
  return spans.map(raw => {
    const span = { cols: Math.min(raw.cols, cols), rows: raw.rows }
    for (let row = 0; ; row++) {
      for (let col = 0; col + span.cols <= cols; col++) {
        if (!free(row, col, span)) continue
        for (let r = row; r < row + span.rows; r++) {
          taken[r] ??= []
          for (let c = col; c < col + span.cols; c++) taken[r][c] = true
        }
        return { ...span, row: row + 1, col: col + 1 }
      }
    }
  })
}

/** 0-based occupancy grid of placements: tile index per cell, -1 when empty. */
function occupancy(placed: Placement[], columns: number): number[][] {
  const lastRow = placed.reduce((max, p) => Math.max(max, p.row + p.rows - 1), 0)
  const grid = Array.from({ length: lastRow }, () => Array<number>(columns).fill(-1))
  placed.forEach((p, index) => {
    for (let r = p.row - 1; r < p.row - 1 + p.rows; r++) {
      for (let c = p.col - 1; c < p.col - 1 + p.cols; c++) grid[r][c] = index
    }
  })
  return grid
}

/** The first empty cell above the last occupied row (0-based), if any. */
export function firstHole(placed: Placement[], columns: number): [number, number] | null {
  const grid = occupancy(placed, columns)
  for (let r = 0; r < grid.length - 1; r++) {
    for (let c = 0; c < columns; c++) if (grid[r][c] < 0) return [r, c]
  }
  return null
}

/** Tallest and widest a tile may grow to while filling gaps. */
const MAX_GROWN_ROWS = 3

/**
 * Fills one gap at (row, col) by growing a neighbouring tile into it: the tile on the left
 * widens, else the tile on the right widens to the left, else the tile above grows down. A tile
 * only grows when the whole new column or row strip is empty. Returns false when none can.
 */
function growInto(placed: Placement[], columns: number, row: number, col: number): boolean {
  const grid = occupancy(placed, columns)
  const emptyColumn = (c: number, p: Placement) => {
    for (let r = p.row - 1; r < p.row - 1 + p.rows; r++) if (grid[r]?.[c] !== -1) return false
    return true
  }
  const emptyRow = (r: number, p: Placement) => {
    if (r >= grid.length) return false
    for (let c = p.col - 1; c < p.col - 1 + p.cols; c++) if (grid[r][c] !== -1) return false
    return true
  }
  const left = col > 0 ? grid[row][col - 1] : -1
  if (left >= 0 && emptyColumn(col, placed[left])) {
    placed[left] = { ...placed[left], cols: placed[left].cols + 1 }
    return true
  }
  const right = col + 1 < columns ? grid[row][col + 1] : -1
  if (right >= 0 && emptyColumn(col, placed[right])) {
    placed[right] = { ...placed[right], col: placed[right].col - 1, cols: placed[right].cols + 1 }
    return true
  }
  const above = row > 0 ? grid[row - 1][col] : -1
  if (above >= 0 && placed[above].rows < MAX_GROWN_ROWS && emptyRow(row, placed[above])) {
    placed[above] = { ...placed[above], rows: placed[above].rows + 1 }
    return true
  }
  return false
}

/**
 * The full layout: sizes from [`wallSpans`], dense packing, then gaps above the last row are
 * filled by growing neighbours. When a gap can't be filled that way, the last tile that is not
 * 1x1 is made 1x1 and the wall is packed again (all 1x1 always packs without gaps), so the
 * result always has no gap above the last row. Deterministic.
 */
export function layoutWall(items: WallItem[], columns: number): Placement[] {
  const cols = Math.max(1, Math.floor(columns))
  const spans = wallSpans(items, cols)
  for (;;) {
    const placed = placeTiles(spans, cols)
    let hole = firstHole(placed, cols)
    while (hole && growInto(placed, cols, hole[0], hole[1])) hole = firstHole(placed, cols)
    if (!hole) return placed
    let last = -1
    for (let i = spans.length - 1; i >= 0; i--) {
      if (spans[i].cols > 1 || spans[i].rows > 1) { last = i; break }
    }
    if (last < 0) return placed
    spans[last] = { cols: 1, rows: 1 }
  }
}

/** Tile indexes in reading order (top to bottom, left to right), as the user sees them. */
export function visualOrder(placements: Placement[]): number[] {
  return placements
    .map((place, index) => ({ place, index }))
    .sort((a, b) => a.place.row - b.place.row || a.place.col - b.place.col)
    .map(entry => entry.index)
}
