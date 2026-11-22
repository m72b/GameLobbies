//! Parsing of Wikidata, Wikipedia and SPARQL responses (pure). Every value is checked: ids
//! against strict patterns, text through the IGDB sanitizers, dates against their precision.

use super::http::WikiError;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Q-ids: `Q` and up to 12 digits, no leading zero.
static QID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Q[1-9][0-9]{0,11}$").unwrap());
static STEAM_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[1-9][0-9]{0,11}$").unwrap());
static SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9_\-./]{0,99}$").unwrap());
static TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\+([0-9]{4})-([0-9]{2})-([0-9]{2})T00:00:00Z$").unwrap());

/// Values kept per property (platform lists of old games run into dozens).
pub const MAX_PER_PROPERTY: usize = 12;
const MAX_FILES_PER_PROPERTY: usize = 3;

pub fn valid_qid(raw: &str) -> bool {
    QID.is_match(raw)
}

pub fn valid_steam_id(raw: &str) -> bool {
    STEAM_ID.is_match(raw)
}

/// IGDB, GOG and Epic slugs as Wikidata stores them (`papers-please`, `game/papers_please`).
pub fn valid_slug(raw: &str) -> bool {
    SLUG.is_match(raw) && !raw.contains("..")
}

/// Wikidata's English Wikipedia sitelink title: plain text, sane length.
fn valid_article_title(raw: &str) -> Option<String> {
    let title = crate::igdb::clean_name(raw)?;
    (title.len() <= 255 && !title.contains(['|', '#', '<', '>', '[', ']', '{', '}']))
        .then_some(title)
}

/// A release date at the precision Wikidata gives it: `YYYY-MM-DD`, `YYYY-MM` or `YYYY`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Release {
    /// Precision: 11 day, 10 month, 9 year.
    pub precision: u8,
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Release {
    pub fn text(&self) -> String {
        match self.precision {
            11 => format!("{:04}-{:02}-{:02}", self.year, self.month, self.day),
            10 => format!("{:04}-{:02}", self.year, self.month),
            _ => format!("{:04}", self.year),
        }
    }
}

/// One Wikidata item with the claims Game Lobby uses.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entity {
    pub qid: String,
    pub label: Option<String>,
    pub description: Option<String>,
    pub aliases: Vec<String>,
    /// The English Wikipedia article (sitelink).
    pub enwiki: Option<String>,
    /// P31 instance of.
    pub classes: Vec<String>,
    /// P629 edition or translation of.
    pub edition_of: Vec<String>,
    /// P577, see [`best_release`].
    pub release: Option<Release>,
    pub platforms: Vec<String>,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub genres: Vec<String>,
    pub steam: Vec<String>,
    pub gog: Vec<String>,
    pub epic: Vec<String>,
    pub igdb: Vec<String>,
    pub mobygames: Vec<String>,
    /// P154 logo files, P18 image files (Commons file names).
    pub logos: Vec<String>,
    pub images: Vec<String>,
}

/// The statements of a property worth using: the preferred ones when there are any, else the
/// normal ones; deprecated statements and "no value"/"unknown value" snaks never.
fn best_statements<'a>(claims: &'a Value, property: &str) -> Vec<&'a Value> {
    let all: Vec<&Value> = claims
        .get(property)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|s| {
            s.get("mainsnak")
                .and_then(|m| m.get("snaktype"))
                .and_then(Value::as_str)
                == Some("value")
        })
        .collect();
    fn rank(s: &Value) -> &str {
        s.get("rank").and_then(Value::as_str).unwrap_or("normal")
    }
    let preferred: Vec<&Value> = all
        .iter()
        .copied()
        .filter(|s| rank(s) == "preferred")
        .collect();
    if !preferred.is_empty() {
        return preferred;
    }
    all.into_iter().filter(|s| rank(s) == "normal").collect()
}

fn datavalue(statement: &Value) -> Option<&Value> {
    statement.get("mainsnak")?.get("datavalue")?.get("value")
}

fn item_ids(claims: &Value, property: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in best_statements(claims, property) {
        if let Some(id) = datavalue(s)
            .and_then(|v| v.get("id"))
            .and_then(Value::as_str)
            .filter(|id| valid_qid(id))
        {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
        if out.len() >= MAX_PER_PROPERTY {
            break;
        }
    }
    out
}

fn strings(claims: &Value, property: &str, valid: fn(&str) -> bool, max: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in best_statements(claims, property) {
        if let Some(v) = datavalue(s).and_then(Value::as_str).map(str::trim) {
            if valid(v) && !out.iter().any(|o| o == v) {
                out.push(v.to_string());
            }
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

fn valid_file(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= 240
        && !raw.contains(['|', '#', '<', '>', '[', ']', '{', '}', '/'])
}

/// Parses a Wikidata time value (`+2024-01-26T00:00:00Z`, precision 9–11).
pub fn parse_time(time: &str, precision: u64) -> Option<Release> {
    let caps = TIME.captures(time)?;
    let year: i32 = caps[1].parse().ok()?;
    let month: u8 = caps[2].parse().ok()?;
    let day: u8 = caps[3].parse().ok()?;
    if !(1900..=2200).contains(&year) {
        return None;
    }
    let precision = precision as u8;
    let ok = match precision {
        11 => (1..=12).contains(&month) && (1..=31).contains(&day),
        10 => (1..=12).contains(&month),
        9 => true,
        _ => false,
    };
    ok.then_some(Release {
        precision,
        year,
        month: if precision >= 10 { month } else { 0 },
        day: if precision == 11 { day } else { 0 },
    })
}

/// The first release: among the best-ranked P577 statements, the most precise ones win (so a
/// stray year-only or month-only claim never beats a full date), then the earliest of those.
pub fn best_release(claims: &Value) -> Option<Release> {
    let dates: Vec<Release> = best_statements(claims, "P577")
        .into_iter()
        .filter_map(|s| {
            let v = datavalue(s)?;
            parse_time(v.get("time")?.as_str()?, v.get("precision")?.as_u64()?)
        })
        .collect();
    let precision = dates.iter().map(|d| d.precision).max()?;
    dates
        .into_iter()
        .filter(|d| d.precision == precision)
        .min_by_key(|d| (d.year, d.month, d.day))
}

/// `labels.en.value` (with `languagefallback` this may be the language-neutral `mul` label).
fn text_of(entity: &Value, field: &str) -> Option<String> {
    entity
        .get(field)
        .and_then(|l| l.get("en").or_else(|| l.get("mul")))
        .and_then(|l| l.get("value"))
        .and_then(Value::as_str)
        .and_then(crate::igdb::clean_name)
}

fn aliases(entity: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for lang in ["en", "mul"] {
        for a in entity
            .get("aliases")
            .and_then(|a| a.get(lang))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(name) = a
                .get("value")
                .and_then(Value::as_str)
                .and_then(crate::igdb::clean_name)
            {
                if !out.contains(&name) && out.len() < 20 {
                    out.push(name);
                }
            }
        }
    }
    out
}

pub fn parse_entity(entity: &Value) -> Option<Entity> {
    let qid = entity.get("id").and_then(Value::as_str)?;
    if !valid_qid(qid) || entity.get("missing").is_some() {
        return None;
    }
    let empty = Value::Null;
    let claims = entity.get("claims").unwrap_or(&empty);
    let enwiki = entity
        .get("sitelinks")
        .and_then(|s| s.get("enwiki"))
        .and_then(|s| s.get("title"))
        .and_then(Value::as_str)
        .and_then(valid_article_title);
    Some(Entity {
        qid: qid.to_string(),
        label: text_of(entity, "labels"),
        description: text_of(entity, "descriptions"),
        aliases: aliases(entity),
        enwiki,
        classes: item_ids(claims, "P31"),
        edition_of: item_ids(claims, "P629"),
        release: best_release(claims),
        platforms: item_ids(claims, "P400"),
        developers: item_ids(claims, "P178"),
        publishers: item_ids(claims, "P123"),
        genres: item_ids(claims, "P136"),
        steam: strings(claims, "P1733", valid_steam_id, MAX_PER_PROPERTY),
        gog: strings(claims, "P2725", valid_slug, MAX_PER_PROPERTY),
        epic: strings(claims, "P6278", valid_slug, MAX_PER_PROPERTY),
        igdb: strings(claims, "P5794", valid_slug, MAX_PER_PROPERTY),
        mobygames: strings(claims, "P11688", valid_slug, MAX_PER_PROPERTY),
        logos: strings(claims, "P154", valid_file, MAX_FILES_PER_PROPERTY),
        images: strings(claims, "P18", valid_file, MAX_FILES_PER_PROPERTY),
    })
}

fn json(body: &str) -> Result<Value, WikiError> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| WikiError::Invalid("the response is not JSON."))?;
    if let Some(code) = value
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
    {
        return Err(WikiError::Api(code.chars().take(40).collect()));
    }
    Ok(value)
}

/// `wbgetentities` → the items, keyed by the id asked for (a redirected item is returned under
/// the id that was asked for, with its new id in `qid`).
pub fn parse_entities(body: &str) -> Result<HashMap<String, Entity>, WikiError> {
    let value = json(body)?;
    let entities = value
        .get("entities")
        .and_then(Value::as_object)
        .ok_or(WikiError::Invalid("no entities."))?;
    Ok(entities
        .iter()
        .filter(|(key, _)| valid_qid(key))
        .filter_map(|(key, e)| parse_entity(e).map(|e| (key.clone(), e)))
        .collect())
}

/// `wbgetentities&props=labels` → English (or language-neutral) label by id.
pub fn parse_labels(body: &str) -> Result<HashMap<String, String>, WikiError> {
    let value = json(body)?;
    let entities = value
        .get("entities")
        .and_then(Value::as_object)
        .ok_or(WikiError::Invalid("no entities."))?;
    Ok(entities
        .iter()
        .filter(|(key, _)| valid_qid(key))
        .filter_map(|(key, e)| text_of(e, "labels").map(|l| (key.clone(), l)))
        .collect())
}

/// One `wbsearchentities` hit.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub qid: String,
    pub label: Option<String>,
    pub description: Option<String>,
}

pub fn parse_search(body: &str) -> Result<Vec<SearchHit>, WikiError> {
    let value = json(body)?;
    Ok(value
        .get("search")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hit| {
            let qid = hit
                .get("id")
                .and_then(Value::as_str)
                .filter(|q| valid_qid(q))?;
            Some(SearchHit {
                qid: qid.to_string(),
                label: hit
                    .get("label")
                    .and_then(Value::as_str)
                    .and_then(crate::igdb::clean_name),
                description: hit
                    .get("description")
                    .and_then(Value::as_str)
                    .and_then(crate::igdb::clean_name),
            })
        })
        .collect())
}

/// Which external id a SPARQL row matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdKind {
    Steam,
    Igdb,
}

/// SPARQL JSON results with `?i` (the item) and `?steam` or `?igdb` → (kind, id, Q-id) rows.
pub fn parse_sparql(body: &str) -> Result<Vec<(IdKind, String, String)>, WikiError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| WikiError::Invalid("the SPARQL response is not JSON."))?;
    let bindings = value
        .get("results")
        .and_then(|r| r.get("bindings"))
        .and_then(Value::as_array)
        .ok_or(WikiError::Invalid("no SPARQL results."))?;
    let get = |row: &Value, var: &str| {
        row.get(var)
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut rows = Vec::new();
    for row in bindings {
        let Some(qid) = get(row, "i")
            .and_then(|uri| {
                uri.strip_prefix("http://www.wikidata.org/entity/")
                    .map(str::to_string)
            })
            .filter(|q| valid_qid(q))
        else {
            continue;
        };
        if let Some(steam) = get(row, "steam").filter(|s| valid_steam_id(s)) {
            rows.push((IdKind::Steam, steam, qid.clone()));
        }
        if let Some(igdb) = get(row, "igdb").filter(|s| valid_slug(s)) {
            rows.push((IdKind::Igdb, igdb, qid));
        }
    }
    rows.sort();
    rows.dedup();
    Ok(rows)
}

impl PartialOrd for IdKind {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IdKind {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

/// The batched id lookup: every item whose Steam app id (P1733) or IGDB id (P5794) is one of
/// the given values. Values are validated again here, so nothing else can reach the query.
pub fn sparql_query(steam: &[String], igdb: &[String]) -> Option<String> {
    let steam: Vec<String> = steam
        .iter()
        .filter(|s| valid_steam_id(s))
        .map(|s| format!("\"{s}\""))
        .collect();
    let igdb: Vec<String> = igdb
        .iter()
        .filter(|s| valid_slug(s) && !s.contains('"') && !s.contains('\\'))
        .map(|s| format!("\"{s}\""))
        .collect();
    let mut parts = Vec::new();
    if !steam.is_empty() {
        parts.push(format!(
            "{{ VALUES ?steam {{ {} }} ?i wdt:P1733 ?steam . }}",
            steam.join(" ")
        ));
    }
    if !igdb.is_empty() {
        parts.push(format!(
            "{{ VALUES ?igdb {{ {} }} ?i wdt:P5794 ?igdb . }}",
            igdb.join(" ")
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "SELECT ?i ?steam ?igdb WHERE {{ {} }} LIMIT 2000",
        parts.join(" UNION ")
    ))
}

/// The first paragraph of a Wikipedia lead section.
#[derive(Debug, Clone, PartialEq)]
pub struct Extract {
    /// The article's final title (after redirects).
    pub title: String,
    pub text: String,
}

/// Longest description kept (about a long paragraph).
pub const MAX_EXTRACT_BYTES: usize = 1500;
/// Shorter "paragraphs" are stubs or disambiguation lines and are not shown.
const MIN_EXTRACT_CHARS: usize = 40;

/// The first paragraph of a plain-text lead, sanitized, cut at a sentence end when long.
pub fn first_paragraph(extract: &str) -> Option<String> {
    let para = extract.split('\n').map(str::trim).find(|p| !p.is_empty())?;
    let text = crate::igdb::clean_text(para, MAX_EXTRACT_BYTES * 2)?.replace('\n', " ");
    let text = if text.len() > MAX_EXTRACT_BYTES {
        let mut end = MAX_EXTRACT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let cut = &text[..end];
        match cut.rfind(". ") {
            Some(i) if i > MAX_EXTRACT_BYTES / 2 => cut[..=i].to_string(),
            _ => format!("{}…", cut.trim_end()),
        }
    } else {
        text
    };
    (text.chars().count() >= MIN_EXTRACT_CHARS).then_some(text)
}

/// `action=query&prop=extracts|pageprops` (formatversion 2) → the extract per requested title
/// (following `normalized` and `redirects`); disambiguation and missing pages are left out.
pub fn parse_extracts(body: &str) -> Result<HashMap<String, Extract>, WikiError> {
    let value = json(body)?;
    let query = value
        .get("query")
        .ok_or(WikiError::Invalid("no query result."))?;
    let pairs = |key: &str| -> Vec<(String, String)> {
        query
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    p.get("from")?.as_str()?.to_string(),
                    p.get("to")?.as_str()?.to_string(),
                ))
            })
            .collect()
    };
    let normalized = pairs("normalized");
    let redirects = pairs("redirects");
    let mut pages: HashMap<String, Extract> = HashMap::new();
    for page in query
        .get("pages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if page.get("missing").is_some()
            || page.get("invalid").is_some()
            || page
                .get("pageprops")
                .and_then(|p| p.get("disambiguation"))
                .is_some()
        {
            continue;
        }
        let Some(title) = page
            .get("title")
            .and_then(Value::as_str)
            .and_then(valid_article_title)
        else {
            continue;
        };
        let Some(text) = page
            .get("extract")
            .and_then(Value::as_str)
            .and_then(first_paragraph)
        else {
            continue;
        };
        pages.insert(title.clone(), Extract { title, text });
    }
    // Map each requested title through normalization and redirects to its page.
    let mut out = pages.clone();
    for (from, to) in normalized.iter().chain(redirects.iter()) {
        let mut target = to.clone();
        if let Some((_, next)) = redirects.iter().find(|(f, _)| *f == target) {
            target = next.clone();
        }
        if let Some(page) = pages.get(&target) {
            out.insert(from.clone(), page.clone());
        }
    }
    for (from, to) in &normalized {
        if let Some((_, next)) = redirects.iter().find(|(f, _)| f == to) {
            if let Some(page) = pages.get(next) {
                out.insert(from.clone(), page.clone());
            }
        }
    }
    Ok(out)
}

/// `https://en.wikipedia.org/wiki/<Title_with_underscores>`, path-encoded.
pub fn article_url(title: &str) -> String {
    let mut url = url::Url::parse("https://en.wikipedia.org/wiki/").unwrap();
    url.path_segments_mut()
        .expect("has a path")
        .pop()
        .push(&title.replace(' ', "_"));
    url.to_string()
}

/// `https://www.wikidata.org/wiki/Q…` for a valid id.
pub fn item_url(qid: &str) -> Option<String> {
    valid_qid(qid).then(|| format!("https://www.wikidata.org/wiki/{qid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ENTITIES: &str = include_str!("testdata/entities.json");
    const LABELS: &str = include_str!("testdata/labels.json");
    const SEARCH: &str = include_str!("testdata/search_doom.json");
    const SPARQL: &str = include_str!("testdata/sparql.json");
    const EXTRACTS: &str = include_str!("testdata/extracts.json");

    #[test]
    fn entities_parse_with_best_ranks_and_clean_values() {
        let entities = parse_entities(ENTITIES).unwrap();
        let hl2 = &entities["Q193581"];
        assert_eq!(hl2.label.as_deref(), Some("Half-Life 2"));
        assert_eq!(hl2.enwiki.as_deref(), Some("Half-Life 2"));
        assert_eq!(hl2.classes, vec!["Q7889"]);
        assert_eq!(hl2.steam, vec!["220"]);
        assert_eq!(hl2.igdb, vec!["half-life-2"]);
        assert_eq!(hl2.developers, vec!["Q193559"]);
        assert_eq!(hl2.logos, vec!["Half-Life 2 text logo.svg"]);
        assert_eq!(hl2.release.as_ref().unwrap().text(), "2004-11-16");
        // A deprecated statement and a "no value" snak are skipped; a bad id is refused.
        assert!(!hl2.platforms.contains(&"Q999999999".to_string()));
        assert!(!hl2.genres.iter().any(|g| g.contains("evil")));
        // Labels only in `mul` still come through.
        let doom = &entities["Q189784"];
        assert_eq!(doom.label.as_deref(), Some("Doom"));
        assert_eq!(doom.release.as_ref().unwrap().text(), "1993-12-10");
        assert_eq!(doom.gog, vec!["game/the_ultimate_doom", "game/doom_1993"]);
        // An edition points at its game.
        let goty = &entities["Q108022844"];
        assert_eq!(goty.classes, vec!["Q60997816"]);
        assert_eq!(goty.edition_of, vec!["Q221164"]);
        // A missing item is left out.
        assert!(!entities.contains_key("Q4"));
    }

    #[test]
    fn release_dates_prefer_precise_and_early_claims() {
        let claims = |dates: &[(&str, u64, &str)]| {
            json!({ "P577": dates.iter().map(|(t, p, rank)| json!({
                "rank": rank,
                "mainsnak": {"snaktype": "value", "datavalue": {"value": {"time": t, "precision": p}}}
            })).collect::<Vec<_>>() })
        };
        // Tears of the Kingdom: stray year-only claims lose to the full date.
        let totk = claims(&[
            ("+2022-00-00T00:00:00Z", 9, "normal"),
            ("+2023-00-00T00:00:00Z", 9, "normal"),
            ("+2023-05-12T00:00:00Z", 11, "normal"),
        ]);
        assert_eq!(best_release(&totk).unwrap().text(), "2023-05-12");
        // The earliest full date wins.
        let cairn = claims(&[
            ("+2026-01-29T00:00:00Z", 11, "normal"),
            ("+2026-01-01T00:00:00Z", 11, "normal"),
            ("+2027-00-00T00:00:00Z", 9, "normal"),
        ]);
        assert_eq!(best_release(&cairn).unwrap().text(), "2026-01-01");
        // Preferred rank wins over earlier normal claims; deprecated ones are ignored.
        let ranked = claims(&[
            ("+2001-03-04T00:00:00Z", 11, "normal"),
            ("+2003-05-06T00:00:00Z", 11, "preferred"),
            ("+1999-01-01T00:00:00Z", 11, "deprecated"),
        ]);
        assert_eq!(best_release(&ranked).unwrap().text(), "2003-05-06");
        // Year and month precision are kept as such.
        assert_eq!(
            best_release(&claims(&[("+1985-00-00T00:00:00Z", 9, "normal")]))
                .unwrap()
                .text(),
            "1985"
        );
        assert_eq!(
            best_release(&claims(&[("+2003-11-00T00:00:00Z", 10, "normal")]))
                .unwrap()
                .text(),
            "2003-11"
        );
        // Out of range, decade precision, odd strings: none.
        for (t, p) in [
            ("+1200-01-01T00:00:00Z", 11),
            ("+1990-00-00T00:00:00Z", 8),
            ("+2020-13-01T00:00:00Z", 11),
            ("-2020-01-01T00:00:00Z", 11),
            ("2020-01-01", 11),
        ] {
            assert_eq!(parse_time(t, p), None, "{t}");
        }
    }

    #[test]
    fn labels_search_and_sparql_parse() {
        let labels = parse_labels(LABELS).unwrap();
        assert_eq!(labels["Q193559"], "Valve Corporation");
        assert_eq!(labels["Q19610114"], "Nintendo Switch");
        assert!(!labels.contains_key("Q0"));
        let hits = parse_search(SEARCH).unwrap();
        assert_eq!(hits[0].qid, "Q189784");
        assert_eq!(hits[0].description.as_deref(), Some("1993 video game"));
        assert!(hits.iter().all(|h| valid_qid(&h.qid)));
        let rows = parse_sparql(SPARQL).unwrap();
        assert!(rows.contains(&(IdKind::Steam, "220".into(), "Q193581".into())));
        // One Steam id on two items: both rows are kept (the caller never guesses).
        assert_eq!(
            rows.iter()
                .filter(|(k, id, _)| *k == IdKind::Steam && id == "22370")
                .count(),
            2
        );
        assert!(rows.contains(&(IdKind::Igdb, "doom".into(), "Q189784".into())));
        // Rows with odd values are dropped.
        assert!(!rows.iter().any(|(_, id, _)| id.contains(' ')));
        assert!(parse_sparql("<html>").is_err());
        assert!(matches!(
            parse_entities(r#"{"error":{"code":"maxlag","info":"x"}}"#),
            Err(WikiError::Api(code)) if code == "maxlag"
        ));
    }

    #[test]
    fn sparql_queries_take_only_valid_ids() {
        let q = sparql_query(
            &["220".into(), "22\" } DROP".into(), "0".into()],
            &["half-life-2".into(), "x\"y".into(), "a b".into()],
        )
        .unwrap();
        assert_eq!(
            q,
            "SELECT ?i ?steam ?igdb WHERE { { VALUES ?steam { \"220\" } ?i wdt:P1733 ?steam . } UNION { VALUES ?igdb { \"half-life-2\" } ?i wdt:P5794 ?igdb . } } LIMIT 2000"
        );
        assert_eq!(sparql_query(&["abc".into()], &[]), None);
    }

    #[test]
    fn extracts_follow_redirects_and_keep_the_first_paragraph() {
        let extracts = parse_extracts(EXTRACTS).unwrap();
        let doom = &extracts["Doom (1993 video game)"];
        assert_eq!(doom.title, "Doom (1993 video game)");
        assert!(doom.text.starts_with("Doom is a 1993 first-person shooter"));
        assert!(!doom.text.contains('\n'));
        // Asked for under a redirect and a non-normalized title.
        assert_eq!(extracts["Half-life 2"].title, "Half-Life 2");
        assert_eq!(extracts["HL2"].title, "Half-Life 2");
        // Disambiguation and missing pages are left out.
        assert!(!extracts.contains_key("Doom"));
        assert!(!extracts.contains_key("No such game"));
        assert_eq!(first_paragraph("\n\nShort.\nMore"), None);
        let long = format!("{} End.", "A sentence here. ".repeat(200));
        let cut = first_paragraph(&long).unwrap();
        assert!(cut.len() <= MAX_EXTRACT_BYTES && cut.ends_with('.'));
        assert_eq!(
            article_url("Doom (1993 video game)"),
            "https://en.wikipedia.org/wiki/Doom_(1993_video_game)"
        );
        assert_eq!(
            article_url("Papers, Please?"),
            "https://en.wikipedia.org/wiki/Papers,_Please%3F"
        );
        assert_eq!(
            item_url("Q42").as_deref(),
            Some("https://www.wikidata.org/wiki/Q42")
        );
        assert_eq!(item_url("Q42/../x"), None);
    }

    #[test]
    fn ids_are_strict() {
        assert!(valid_qid("Q7889") && !valid_qid("Q0") && !valid_qid("q7889") && !valid_qid("Q1 "));
        assert!(valid_steam_id("1778820") && !valid_steam_id("01") && !valid_steam_id("1e3"));
        assert!(valid_slug("game/papers_please") && valid_slug("cairn--3"));
        assert!(!valid_slug("a b") && !valid_slug("../x") && !valid_slug("A") && !valid_slug(""));
    }
}
