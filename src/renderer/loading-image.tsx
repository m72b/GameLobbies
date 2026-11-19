import { useEffect, useLayoutEffect, useRef, useState, type ImgHTMLAttributes } from 'react'

/** Images that are already in the cache show before this and never get a spinner. */
const SPINNER_DELAY_MS = 100

/**
 * The one loading indicator for covers, heroes, logos and previews: a small, low-contrast ring in
 * the theme accent, centred in its (positioned) parent. It only appears when loading takes longer
 * than {@link SPINNER_DELAY_MS}, so cached images do not flicker.
 */
export function Spinner() {
  const [visible, setVisible] = useState(false)
  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(true), SPINNER_DELAY_MS)
    return () => window.clearTimeout(timer)
  }, [])
  return visible ? <span className="img-spinner" role="presentation" aria-hidden="true" /> : null
}

/**
 * An `<img>` that stays invisible with a {@link Spinner} over it until it has loaded, then fades
 * in. An already-cached image shows at once. `onError` is passed through so callers can fall back;
 * remount it (`key`) when `src` changes.
 */
export function FadeImg({ className, onLoad, onError, ...rest }: ImgHTMLAttributes<HTMLImageElement>) {
  const ref = useRef<HTMLImageElement>(null)
  const [state, setState] = useState<'loading' | 'ready' | 'error'>('loading')
  useLayoutEffect(() => {
    const image = ref.current
    if (image?.complete && image.naturalWidth > 0) setState('ready')
  }, [])
  return <>
    <img {...rest} ref={ref} className={(className ? className + ' ' : '') + 'fade-img' + (state === 'ready' ? ' ready' : '')}
      onLoad={event => { setState('ready'); onLoad?.(event) }}
      onError={event => { setState('error'); onError?.(event) }} />
    {state === 'loading' && <Spinner />}
  </>
}
