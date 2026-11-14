// Run with `npm run test:web` (Node strips the TypeScript types of sessions.ts).
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { backloggdSearchUrl, durationSeconds, editProblem, formatRange, fromLocalInput, MAX_SESSION_MS, noteLength, toLocalInput, utcOffsetMinutes } from './sessions.ts'

const NOW = new Date(2026, 10, 14, 20, 0).getTime()
const clock = ms => { const d = new Date(ms); return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0') }

test('datetime-local values round trip in local time', () => {
  const ms = new Date(2026, 10, 14, 18, 5).getTime()
  assert.equal(toLocalInput(ms), '2026-11-14T18:05')
  assert.equal(fromLocalInput('2026-11-14T18:05'), ms)
  assert.equal(fromLocalInput('2026-11-14T18:05:30'), ms + 30_000)
  assert.equal(fromLocalInput('2026-02-30T10:00'), null, 'no such day')
  assert.equal(fromLocalInput('yesterday'), null)
  assert.equal(fromLocalInput(''), null)
})

test('corrected times are checked like the backend does', () => {
  const start = new Date(2026, 10, 14, 18, 0).getTime()
  assert.equal(editProblem(start, start + 3_600_000, NOW), null)
  assert.match(editProblem(null, start, NOW), /start and an end/)
  assert.match(editProblem(start, start, NOW), /after the start/)
  assert.match(editProblem(start, start - 60_000, NOW), /after the start/)
  assert.match(editProblem(start, NOW + 60_000, NOW), /future/)
  assert.match(editProblem(1000, 5000, NOW), /2000/)
  assert.match(editProblem(start, start + 500, NOW), /one second/)
  assert.match(editProblem(start - MAX_SESSION_MS - 1000, start, NOW), /30 days/)
  assert.equal(durationSeconds(start, start + 7_620_500), 7620)
})

test('ranges show the end time and a day change', () => {
  const start = new Date(2026, 10, 14, 23, 10).getTime()
  assert.equal(formatRange(start, start + 30 * 60_000, clock), '23:10 – 23:40')
  assert.equal(formatRange(start, start + 2 * 3_600_000, clock), '23:10 – 01:10 (+1 day)')
  assert.equal(formatRange(start, undefined, clock), '23:10 – now')
})

test('notes are counted in characters, not UTF-16 units', () => {
  assert.equal(noteLength('héllo'), 5)
  assert.equal(noteLength('🎮🎮'), 2)
})

test('Backloggd search links encode the title', () => {
  assert.equal(backloggdSearchUrl(' Hollow Knight '), 'https://backloggd.com/search/games/Hollow%20Knight')
  assert.equal(backloggdSearchUrl('Half-Life 2: Episode One / ?#'), 'https://backloggd.com/search/games/Half-Life%202%3A%20Episode%20One%20%2F%20%3F%23')
  assert.ok(backloggdSearchUrl('x'.repeat(500)).length < 260)
})

test('UTC offsets follow the browser time zone', () => {
  assert.equal(utcOffsetMinutes(NOW), -new Date(NOW).getTimezoneOffset())
})
