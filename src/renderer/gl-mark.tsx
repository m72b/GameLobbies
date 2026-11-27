// The app icon (src-tauri/app-icon.svg): Roboto Bold "GL" outlines with its green gradient, no tile.
// Keep the path and gradient in step with the app icon if the icon changes.
import { useId } from 'react'

const PATH = "M512.4 491.3L512.4 706.0Q482.4 741.9 427.8 761.6Q373.1 781.4 306.6 781.4L306.6 781.4Q236.7 781.4 184.1 750.9Q131.4 720.4 102.8 662.4Q74.1 604.4 73.4 526.1L73.4 526.1L73.4 489.5Q73.4 408.9 100.6 350.0Q127.7 291.1 178.9 259.8Q230.1 228.6 298.8 228.6L298.8 228.6Q394.5 228.6 448.4 274.2Q502.4 319.9 512.4 407.1L512.4 407.1L404.5 407.1Q397.1 360.9 371.8 339.5Q346.5 318.0 302.1 318.0L302.1 318.0Q245.6 318.0 216.0 360.5Q186.5 403.0 186.1 486.9L186.1 486.9L186.1 521.3Q186.1 605.9 218.2 649.1Q250.4 692.3 312.5 692.3L312.5 692.3Q374.9 692.3 401.5 665.7L401.5 665.7L401.5 573.0L300.6 573.0L300.6 491.3L512.4 491.3ZM715.2 236L715.2 684.9L950.6 684.9L950.6 774L604.4 774L604.4 236L715.2 236Z"

export function GlMark({ size = 18, className }: { size?: number; className?: string }) {
  const id = 'gl-' + useId().replace(/:/g, '')
  return (
    <svg className={className} width={size} height={size} viewBox="64 56 896 896" aria-hidden="true" focusable="false">
      <defs>
        <linearGradient id={id} gradientUnits="userSpaceOnUse" x1="250" y1="180" x2="800" y2="860">
          <stop offset="0" stopColor="#86efac" />
          <stop offset="0.5" stopColor="#34d17a" />
          <stop offset="1" stopColor="#16a34a" />
        </linearGradient>
      </defs>
      <path fill={'url(#' + id + ')'} d={PATH} />
    </svg>
  )
}
