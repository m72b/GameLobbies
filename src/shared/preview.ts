export type PreviewKind = 'image' | 'video'

/** Classifies one preview URL. Videos (.webm) need a <video>; they are hidden unless `animated`. */
export function previewKind(thumb: string, animated: boolean): PreviewKind | null {
  let path: string
  try { path = new URL(thumb).pathname.toLowerCase() } catch { return null }
  if (path.endsWith('.webm') || path.endsWith('.mp4')) return animated && path.endsWith('.webm') ? 'video' : null
  return 'image'
}

/**
 * What to show for a hero/logo candidate: its thumb, or (when the thumb is a video that can't
 * be played) the full image if that is a still or animated image. Never an <img> pointing at a video.
 */
export function artPreview(cover: { thumb: string; url: string }, animated: boolean): { kind: PreviewKind; src: string } | null {
  for (const src of [cover.thumb, cover.url]) {
    if (!src) continue
    const kind = previewKind(src, animated)
    if (kind) return { kind, src }
  }
  return null
}
