// What the game page's About section shows when IGDB and Wikidata both have something: IGDB
// (and the user's own values) always win; Wikidata and Wikipedia only fill what is missing.
// Tested by about.test.mjs.
import type { IgdbMetadata, WikiImage, WikiMetadata } from './types'

export type FactSource = 'igdb' | 'wikidata'
export interface Fact { value: string; source: FactSource }
export interface AboutData {
  /** IGDB's summary, else the first paragraph of the Wikipedia article. */
  description?: { text: string; source: 'igdb' | 'wikipedia'; url?: string; title?: string }
  developers?: Fact
  publishers?: Fact
  released?: Fact
  platforms?: Fact
  /** Genres to show beside the user's own genre (which always stays first). */
  genres: { values: string[]; source?: FactSource }
  /** Which providers contributed anything (for the credit line). */
  usesIgdb: boolean
  usesWikidata: boolean
  usesWikipedia: boolean
}

const list = (items?: string[]) => items && items.length ? items.join(', ') : undefined
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/** IGDB release dates are UTC midnight (ms); formatted in UTC so the day is right. */
export function formatIgdbDate(ms?: number) {
  if (ms === undefined) return undefined
  const d = new Date(ms)
  return MONTHS[d.getUTCMonth()] + ' ' + d.getUTCDate() + ', ' + d.getUTCFullYear()
}

/** Wikidata dates at their own precision: "Nov 16, 2004", "Nov 2003" or "1985". */
export function formatWikiDate(release?: string) {
  if (!release) return undefined
  const match = /^(\d{4})(?:-(\d{2})(?:-(\d{2}))?)?$/.exec(release)
  if (!match) return undefined
  const [, year, month, day] = match
  const m = month ? MONTHS[Number(month) - 1] : undefined
  if (month && !m) return undefined
  if (day) return m + ' ' + Number(day) + ', ' + year
  if (m) return m + ' ' + year
  return year
}

export function mergeAbout(igdb: IgdbMetadata | undefined, wiki: WikiMetadata | undefined, userGenre?: string): AboutData {
  const pick = (own: string | undefined, other: string | undefined): Fact | undefined =>
    own ? { value: own, source: 'igdb' } : other ? { value: other, source: 'wikidata' } : undefined
  const description = igdb?.summary
    ? { text: igdb.summary, source: 'igdb' as const }
    : wiki?.article ? { text: wiki.article.text, source: 'wikipedia' as const, url: wiki.article.url, title: wiki.article.title } : undefined
  const developers = pick(list(igdb?.developers), list(wiki?.developers))
  const publishers = pick(list(igdb?.publishers), list(wiki?.publishers))
  const released = pick(formatIgdbDate(igdb?.releaseDate), formatWikiDate(wiki?.release))
  const platforms = wiki?.platforms.length ? { value: wiki.platforms.join(', '), source: 'wikidata' as const } : undefined
  const own = userGenre?.trim().toLocaleLowerCase()
  const notOwn = (genres: string[]) => genres.filter(genre => genre.toLocaleLowerCase() !== own)
  const igdbGenres = notOwn(igdb?.genres ?? [])
  const wikiGenres = notOwn(wiki?.genres ?? [])
  const genres = igdbGenres.length ? { values: igdbGenres, source: 'igdb' as const }
    : wikiGenres.length && !own ? { values: wikiGenres, source: 'wikidata' as const } : { values: [] }
  const facts = [developers, publishers, released, platforms]
  return {
    description, developers, publishers, released, platforms, genres,
    usesIgdb: !!igdb,
    usesWikidata: facts.some(fact => fact?.source === 'wikidata') || genres.source === 'wikidata',
    usesWikipedia: description?.source === 'wikipedia'
  }
}

/** "Logo: Valve, Public domain, via Wikimedia Commons" (the artist is left out when unknown). */
export function creditText(image: Pick<WikiImage, 'kind' | 'artist' | 'licence'>) {
  const what = image.kind === 'cover' ? 'Cover' : 'Logo'
  return what + ': ' + [image.artist, image.licence].filter(Boolean).join(', ') + ' via Wikimedia Commons'
}

/** The credit for the image shown, when it came from Commons. */
export function creditFor(credits: WikiImage[] | undefined, shown: string | undefined) {
  return shown === undefined ? undefined : credits?.find(credit => credit.display === shown || credit.src === shown)
}
