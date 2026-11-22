// Run with `npm run test:web` (Node strips the TypeScript types of about.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { creditFor, creditText, formatWikiDate, mergeAbout } from './about.ts'

const wiki = {
  qid: 'Q193581', label: 'Half-Life 2', release: '2004-11-16', platforms: ['Microsoft Windows', 'Xbox'],
  developers: ['Valve Corporation'], publishers: ['Valve Corporation'], genres: ['first-person shooter'],
  steam: ['220'], gog: [], epic: [], igdb: [], mobygames: [],
  article: { title: 'Half-Life 2', url: 'https://en.wikipedia.org/wiki/Half-Life_2', text: 'Half-Life 2 is a 2004 first-person shooter.' }
}
const igdb = { igdbId: 72, name: 'Half-Life 2', summary: 'From IGDB.', genres: ['Shooter'], developers: ['Valve'], publishers: [], releaseDate: Date.UTC(2004, 10, 16) }

test('IGDB values win; Wikidata only fills the gaps', () => {
  const about = mergeAbout(igdb, wiki)
  assert.deepEqual(about.description, { text: 'From IGDB.', source: 'igdb' })
  assert.deepEqual(about.developers, { value: 'Valve', source: 'igdb' })
  // IGDB has no publisher: Wikidata's fills it.
  assert.deepEqual(about.publishers, { value: 'Valve Corporation', source: 'wikidata' })
  assert.deepEqual(about.released, { value: 'Nov 16, 2004', source: 'igdb' })
  assert.deepEqual(about.platforms, { value: 'Microsoft Windows, Xbox', source: 'wikidata' })
  assert.deepEqual(about.genres, { values: ['Shooter'], source: 'igdb' })
  assert.equal(about.usesWikipedia, false)
  assert.equal(about.usesWikidata, true)
})

test('Without IGDB, Wikipedia gives the description with its link', () => {
  const about = mergeAbout(undefined, wiki)
  assert.equal(about.description.source, 'wikipedia')
  assert.equal(about.description.url, 'https://en.wikipedia.org/wiki/Half-Life_2')
  assert.equal(about.released.value, 'Nov 16, 2004')
  assert.deepEqual(about.genres, { values: ['first-person shooter'], source: 'wikidata' })
  assert.equal(about.usesWikipedia, true)
  assert.equal(about.usesIgdb, false)
  // An IGDB match without a summary still gets the Wikipedia description.
  assert.equal(mergeAbout({ ...igdb, summary: undefined }, wiki).description.source, 'wikipedia')
  // Nothing at all.
  const empty = mergeAbout(undefined, undefined)
  assert.equal(empty.description, undefined)
  assert.deepEqual(empty.genres, { values: [] })
})

test("The user's own genre stays; Wikidata genres never add to it", () => {
  assert.deepEqual(mergeAbout(undefined, wiki, 'Action').genres, { values: [] })
  assert.deepEqual(mergeAbout(igdb, wiki, 'shooter').genres, { values: [] })
})

test('Wikidata dates keep their precision', () => {
  assert.equal(formatWikiDate('1993-12-10'), 'Dec 10, 1993')
  assert.equal(formatWikiDate('2003-11'), 'Nov 2003')
  assert.equal(formatWikiDate('1985'), '1985')
  assert.equal(formatWikiDate('2020-13'), undefined)
  assert.equal(formatWikiDate('soon'), undefined)
  assert.equal(formatWikiDate(undefined), undefined)
})

test('Credits name the author and licence and are found by the shown value', () => {
  assert.equal(creditText({ kind: 'logo', artist: 'Valve', licence: 'Public domain' }), 'Logo: Valve, Public domain via Wikimedia Commons')
  assert.equal(creditText({ kind: 'cover', licence: 'CC BY 3.0' }), 'Cover: CC BY 3.0 via Wikimedia Commons')
  const credits = [{ src: 'https://upload.wikimedia.org/wikipedia/commons/a/ab/L.png', display: 'lobby-cover://cache-1.png', kind: 'logo', licence: 'CC0' }]
  assert.equal(creditFor(credits, 'lobby-cover://cache-1.png'), credits[0])
  assert.equal(creditFor(credits, 'https://upload.wikimedia.org/wikipedia/commons/a/ab/L.png'), credits[0])
  assert.equal(creditFor(credits, 'https://cdn2.steamgriddb.com/logo/x.png'), undefined)
  assert.equal(creditFor(undefined, 'x'), undefined)
})
