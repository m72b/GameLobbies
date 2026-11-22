# Built-in Wikidata / Wikipedia / Wikimedia Commons provider

Status: implemented (Unreleased). Code: `src-tauri/src/wikimedia/` (`mod.rs` orchestration and
cache, `http.rs` client, `parse.rs` API parsing, `matching.rs` item choice, `licence.rs` Commons
licence filter, `tests.rs` and `testdata/` fixtures); renderer `src/renderer/wiki-details.tsx`,
`src/shared/about.ts` (merging with IGDB).

## Goal

A fresh install with no keys still gets game facts, logos and, where a free one exists, a cover,
without any user account and without any fair-use or non-free image. IGDB (user Twitch
credentials), SteamGridDB (user key), the Steam CDN and custom art keep working and keep
precedence.

## Sources and licences

| What | Source | Licence | Shown as |
| --- | --- | --- | --- |
| Title, first release, platforms, developers, publishers, genres, external ids (Steam P1733, GOG P2725, Epic P6278, IGDB P5794, MobyGames P11688) | Wikidata | CC0 | "Facts from Wikidata (CC0)" with a link to the item |
| Description: first paragraph of the English article's lead | English Wikipedia (TextExtracts, plain text) | CC BY-SA 4.0 | "From Wikipedia · "Article" · CC BY-SA 4.0" (links); only when IGDB has no description |
| Logos: P154, or P18 when the file name says "logo" | Wikimedia Commons | as on the file page, filtered below | "Logo: <author>, <licence> via Wikimedia Commons" (link to the file page, licence deed link) |
| Covers: P18 when the file name says "cover"/"box" and the original is portrait (height/width 1.2–1.8, at least 350 px tall) | Wikimedia Commons | filtered below | "Cover: <author>, <licence> via Wikimedia Commons" under the poster |

Wikidata has no cover property. Nothing scraped is bundled with the installer; everything is
fetched at run time and cached in the user's library.

### Commons licence policy (`licence.rs`)

An image is used only when all hold:

- The Commons API (`commons.wikimedia.org`) reports the file with `imagerepository: "local"`
  (hosted by Commons). Files that exist only on a Wikipedia, which is where non-free and fair-use
  files live, come back `missing` and are refused.
- `NonFree` is empty (or false).
- `License` is `pd`, `cc0`/`cc-zero`, `cc-by-<version>[-port]` or `cc-by-sa-<version>[-port]`; when
  `License` is empty, `LicenseShortName` must be Public domain, CC0, CC BY [x] or CC BY-SA [x].
  Anything containing NC, ND, fair use or non-free is refused, as are GFDL-only, GPL and anything
  unclear.
- The MIME type is PNG, JPEG, WebP or SVG; SVGs are used only through Commons' PNG rendering.
- The display file (the `thumburl` for 500 px, or the original raster) is
  `https://upload.wikimedia.org/wikipedia/commons/...` with a raster extension. Commons' `utm_`
  tracking parameters are dropped. `/wikipedia/en/` and every other folder are refused.

Stored per image (`wikimedia_images`, keyed by display URL): file name, display URL and size,
original size, licence short name and deed URL (creativecommons.org only), author and credit
(HTML stripped, 200 characters), the Commons file page, AttributionRequired and Restrictions
(shown as a note, for example "trademarked"). The REST `page/summary` endpoint is never used for
images: its thumbnails include non-free files.

## Matching (`matching.rs`)

1. A user's choice (or an earlier automatic match) is fetched by its Q-id and never re-matched.
2. External ids: one SPARQL query per batch of up to 10 games
   (`VALUES ?steam {...} ?i wdt:P1733 ?steam` UNION the same for the IGDB slug of the game's IGDB
   match). Ids are validated (digits; lower-case slugs) before they reach the query. One game
   item is a match. Several items for one id are narrowed only by facts: non-games are dropped and
   an edition (P31 Q60997816 "video game edition" with one P629 "edition or translation of")
   stands for its game. If more than one game is left, the user chooses.
3. Title search (`wbsearchentities`, 10 hits) with the IGDB title cleaner, then only game items
   (P31 in Q7889 video game, Q865493 video game mod, Q61456428 total conversion mod, Q16070115
   video game compilation, Q60997816 video game edition) whose label or alias equals the title
   after normalization (case, punctuation and spacing, `&`/"and", Roman numerals II–IX, edition
   words, a leading "the"). Exactly one is a match; several are split by the game's platform
   (a small checked list of Wikidata platform items), then by the IGDB release year; otherwise the
   candidates (label, Wikidata description, year) are stored and offered in **Find on Wikidata**.

Release date: among best-rank P577 statements (preferred, else normal; never deprecated), the most
precise ones win (so a stray year-only claim never beats a full date), then the earliest.

## HTTP and etiquette (`http.rs`)

- Exact hosts `www.wikidata.org`, `query.wikidata.org`, `en.wikipedia.org`,
  `commons.wikimedia.org`, https only, default port, no credentials; redirects only between those
  hosts, each hop checked, at most two.
- 20 s timeout (10 s connect), 8 MB response cap (also after gzip decoding; `Accept-Encoding: gzip`
  decoded with `flate2`), content type `application/json` (SPARQL also
  `application/sparql-results+json`).
- Serial requests at least 300 ms apart (one limiter for the app), `maxlag=5` on every Action API
  call, `Retry-After` honoured on 429/503/maxlag up to 60 s and two retries (5 s, 20 s without a
  header); longer waits end the request as rate limited.
- User-Agent `GameLobby/<version> (https://github.com/m72b/GameLobby)`; no personal data.
- Batched: entities 5 per call (claims can be large), labels 50, Commons titles 50, Wikipedia
  extracts 20.
- Images never go through this client: they are downloaded by the cover cache, whose allow list
  gained `upload.wikimedia.org` restricted to `/wikipedia/commons/` raster files (size cap, magic
  bytes, downscaling, hashed file names as before).

## Cache and precedence

- `wikimedia_match(gameId, qid, status, matchedBy, source, json, candidates, fetchedAt)`: status
  `matched` / `none` / `ambiguous`; matches stay fresh 30 days, the others 7 days; a newer JSON
  shape refetches. A `user` row is replaced only by another user choice, or refreshed by a fetch of
  the same item; a chosen item that disappears keeps the choice. Not in backups (like the IGDB
  cache `game_metadata`); left empty by the database repair's cache drop; deleted with the game;
  automatic rows are dropped when games are linked or unlinked.
- `wikimedia_images(url, json, fetchedAt)`: credits survive a reset match, so a chosen Commons logo
  always keeps its credit. A chosen Commons logo is stored like a SteamGridDB one (the URL, so it
  survives backups); `validation::page_art_value` accepts exactly Commons display URLs.
- Precedence: IGDB and the user's values first; Wikidata only fills gaps (`src/shared/about.ts`).
  The library's title, genre and cover are never changed by a lookup, except that a game with no
  cover and no default cover gets a Commons cover automatically when one is trusted (original at
  least 600 px tall). Smaller covers (both found in the sample, see below) are offered only, marked
  "Low resolution". Logos are fallbacks behind Steam's and SteamGridDB's; a chosen logo stays first.
- Lookups: when a game page opens (after a running IGDB lookup for that game, so its IGDB id can
  help), and in the background 20 s after start-up and after scans, in batches of 10, stopping when
  the setting is turned off, when Wikimedia rate-limits, or after three failed batches in a row.

## Coverage (sample of 33 games, measured during research)

- Correct item found: 33/33 (Steam id route 12/12).
- Unambiguous after the game-class filter and exact normalized label: 27/33 (ambiguous: Star Wars
  Battlefront II, Ocarina of Time, Doom, Super Mario Bros., Tetris, Fallout 3 GOTY edition; the
  edition case is resolved by P629 and Steam ids, the others go to the user's choice unless a
  platform or IGDB year splits them).
- Facts complete: 28/33.
- Free logos: about 17/33.
- Free vertical covers: 2/33 (Doom box art 271x410, Fallout 3 cropped cover 270x365; low-res Game
  Pass captures with weak provenance), both offered as low-trust candidates and never set
  automatically.
- English Wikipedia lead images: 26/33, all non-free; excluded by design.

## Not done / recommendations

- Only English Wikipedia. Other languages would need their own licence wording and host entries.
- The platform list for tie-breaking covers systems whose Wikidata labels were checked; more could be
  added after checking their Q-ids.
- GOG and Epic ids from Wikidata are stored but not yet matched against library routes (library GOG
  routes carry numeric product ids, Wikidata stores slugs).
