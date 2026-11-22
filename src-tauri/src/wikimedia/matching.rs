//! Choosing the Wikidata item for a library game (pure).
//!
//! - External ids first: an id that leads to exactly one game item is a match. Several items
//!   for one id are narrowed only by facts (not a game, or an edition of another hit); if more
//!   than one is left, the user chooses. Nothing is guessed.
//! - An edition ("video game edition" with "edition or translation of") stands for its game.
//! - Title search: only game items whose label or alias equals the title after normalization
//!   (case, punctuation, spacing, `&`/`and`, Roman numerals, edition words). Exactly one is a
//!   match; several are split by the game's platform, then by a known release year; otherwise
//!   the candidates are offered to the user.

use super::parse::Entity;
use std::collections::HashMap;

/// Item classes treated as games: video game, video game mod, total conversion mod, video game
/// compilation, video game edition (Q-ids checked against Wikidata labels).
pub const GAME_CLASSES: [&str; 5] = ["Q7889", "Q865493", "Q61456428", "Q16070115", EDITION];
/// "video game edition".
pub const EDITION: &str = "Q60997816";

/// Wikidata platform items for catalog systems, for splitting ties only. Only items whose
/// labels were checked are listed; other systems simply don't split ties.
pub fn platform_item(system: &str) -> Option<&'static str> {
    Some(match system {
        "pc" => "Q1406",
        "ps3" => "Q10683",
        "psvita" => "Q188808",
        "psp" => "Q170325",
        "n3ds" => "Q203597",
        "nds" => "Q170323",
        "wiiu" => "Q56942",
        "wii" => "Q8079",
        "gc" => "Q182172",
        "snes" => "Q183259",
        "nes" => "Q172742",
        "gb" => "Q186437",
        "gbc" => "Q203992",
        "n64" => "Q184839",
        _ => return None,
    })
}

/// What was decided for one game.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Match(String),
    /// Several possible items (Q-ids); the user chooses.
    Ambiguous(Vec<String>),
    None,
}

pub fn is_game(entity: &Entity) -> bool {
    entity
        .classes
        .iter()
        .any(|c| GAME_CLASSES.contains(&c.as_str()))
}

/// The game an edition belongs to, when the item is an edition and names exactly one.
pub fn edition_base(entity: &Entity) -> Option<&str> {
    (entity.classes.iter().any(|c| c == EDITION) && entity.edition_of.len() == 1)
        .then(|| entity.edition_of[0].as_str())
}

/// The item to use for `qid`: its game when it is an edition of one we have, else itself.
pub fn resolve<'a>(qid: &'a str, entities: &'a HashMap<String, Entity>) -> &'a str {
    entities
        .get(qid)
        .and_then(edition_base)
        .filter(|base| entities.get(*base).is_some_and(is_game))
        .unwrap_or(qid)
}

fn dedup(list: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for q in list {
        if !out.contains(&q) {
            out.push(q);
        }
    }
    out
}

/// Decides from the items an external id led to.
pub fn pick_by_ids(hits: &[String], entities: &HashMap<String, Entity>) -> Decision {
    // Unknown items (not fetched) are kept: they are facts too, just unchecked.
    let games: Vec<&String> = hits
        .iter()
        .filter(|q| entities.get(*q).is_none_or(is_game))
        .collect();
    let resolved = dedup(
        games
            .iter()
            .map(|q| resolve(q, entities).to_string())
            .collect(),
    );
    match resolved.len() {
        0 => Decision::None,
        1 => Decision::Match(resolved[0].clone()),
        _ => Decision::Ambiguous(resolved),
    }
}

const ROMAN: [(&str, &str); 8] = [
    ("ii", "2"),
    ("iii", "3"),
    ("iv", "4"),
    ("v", "5"),
    ("vi", "6"),
    ("vii", "7"),
    ("viii", "8"),
    ("ix", "9"),
];

/// A title for comparing: edition words and tags removed (as for IGDB searches), lower case,
/// `&` as "and", Roman numerals II–IX as digits, a leading "the" dropped, then letters and
/// digits only (so colon, dash and spacing variants compare equal).
pub fn norm_title(title: &str) -> String {
    let cleaned = crate::igdb::clean_title(title).unwrap_or_else(|| title.to_string());
    let lower = cleaned.to_lowercase().replace('&', " and ");
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            ROMAN
                .iter()
                .find(|(r, _)| *r == w)
                .map_or(w.to_string(), |(_, d)| d.to_string())
        })
        .collect();
    let words = match words.first().map(String::as_str) {
        Some("the") if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    words.concat()
}

fn names(entity: &Entity) -> impl Iterator<Item = &String> {
    entity.label.iter().chain(entity.aliases.iter())
}

/// Decides from title-search candidates (`hits`, best first). `platform` is the game's
/// Wikidata platform item, `year` a known release year (from IGDB).
pub fn pick_by_title(
    title: &str,
    hits: &[String],
    entities: &HashMap<String, Entity>,
    platform: Option<&str>,
    year: Option<i32>,
) -> Decision {
    let wanted = norm_title(title);
    if wanted.is_empty() {
        return Decision::None;
    }
    let exact = dedup(
        hits.iter()
            .filter_map(|q| entities.get(q))
            .filter(|e| is_game(e))
            .filter(|e| names(e).any(|n| norm_title(n) == wanted))
            .map(|e| resolve(&e.qid, entities).to_string())
            .collect(),
    );
    narrow(exact, entities, platform, year)
}

fn narrow(
    candidates: Vec<String>,
    entities: &HashMap<String, Entity>,
    platform: Option<&str>,
    year: Option<i32>,
) -> Decision {
    match candidates.len() {
        0 => return Decision::None,
        1 => return Decision::Match(candidates[0].clone()),
        _ => {}
    }
    let mut left = candidates.clone();
    if let Some(p) = platform {
        let on: Vec<String> = left
            .iter()
            .filter(|q| {
                entities
                    .get(*q)
                    .is_some_and(|e| e.platforms.iter().any(|x| x == p))
            })
            .cloned()
            .collect();
        if !on.is_empty() {
            left = on;
        }
    }
    if left.len() > 1 {
        if let Some(y) = year {
            let same: Vec<String> = left
                .iter()
                .filter(|q| {
                    entities
                        .get(*q)
                        .and_then(|e| e.release.as_ref())
                        .is_some_and(|r| r.year == y)
                })
                .cloned()
                .collect();
            if !same.is_empty() {
                left = same;
            }
        }
    }
    if left.len() == 1 {
        Decision::Match(left[0].clone())
    } else {
        Decision::Ambiguous(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wikimedia::parse::parse_entities;

    fn fixture() -> HashMap<String, Entity> {
        parse_entities(include_str!("testdata/entities.json")).unwrap()
    }

    #[test]
    fn titles_normalize_their_variants() {
        let n = norm_title;
        assert_eq!(n("Star Wars: Battlefront II"), n("Star Wars Battlefront 2"));
        assert_eq!(n("Papers, Please"), n("papers please"));
        assert_eq!(n("Half-Life 2"), n("Half Life 2"));
        assert_eq!(
            n("Kingdom Hearts HD 1.5 + 2.5 ReMIX"),
            n("Kingdom Hearts HD 1.5+2.5 Remix")
        );
        assert_eq!(n("Fallout 3: Game of the Year Edition"), n("Fallout 3"));
        assert_eq!(n("The Witcher 3"), n("Witcher 3"));
        assert_eq!(n("Ratchet & Clank"), n("Ratchet and Clank"));
        assert_eq!(n("DOOM™"), n("Doom"));
        assert_ne!(n("Doom II"), n("Doom"));
        // "X" and "I" are not numerals here (Mega Man X is not Mega Man 10).
        assert_ne!(n("Mega Man X"), n("Mega Man 10"));
        assert_eq!(n("The"), "the");
    }

    #[test]
    fn one_id_hit_matches_and_editions_stand_for_their_game() {
        let e = fixture();
        assert_eq!(
            pick_by_ids(&["Q193581".into()], &e),
            Decision::Match("Q193581".into())
        );
        // Steam 22370 is on the GOTY edition and on Fallout 3: both are Fallout 3.
        assert_eq!(
            pick_by_ids(&["Q108022844".into(), "Q221164".into()], &e),
            Decision::Match("Q221164".into())
        );
        assert_eq!(
            pick_by_ids(&["Q108022844".into()], &e),
            Decision::Match("Q221164".into())
        );
        // Two different games for one id: the user chooses.
        assert_eq!(
            pick_by_ids(&["Q189784".into(), "Q20711315".into()], &e),
            Decision::Ambiguous(vec!["Q189784".into(), "Q20711315".into()])
        );
        // Not a game (a film) is dropped.
        assert_eq!(
            pick_by_ids(&["Q1241843".into(), "Q193581".into()], &e),
            Decision::Match("Q193581".into())
        );
        assert_eq!(pick_by_ids(&["Q1241843".into()], &e), Decision::None);
        assert_eq!(pick_by_ids(&[], &e), Decision::None);
    }

    #[test]
    fn titles_match_only_exact_game_labels_and_ties_need_facts() {
        let e = fixture();
        let doom = ["Q189784".to_string(), "Q20711315".into(), "Q1241843".into()];
        // Two games called Doom (the film is ignored): no platform split (both PC), no year.
        assert_eq!(
            pick_by_title("DOOM", &doom, &e, Some("Q1406"), None),
            Decision::Ambiguous(vec!["Q189784".into(), "Q20711315".into()])
        );
        // A known year splits them.
        assert_eq!(
            pick_by_title("Doom", &doom, &e, Some("Q1406"), Some(2016)),
            Decision::Match("Q20711315".into())
        );
        // A platform only one of them has splits them (DOS for the 1993 game).
        assert_eq!(
            pick_by_title("Doom", &doom, &e, Some("Q170434"), None),
            Decision::Match("Q189784".into())
        );
        // An alias counts; a different name does not.
        assert_eq!(
            pick_by_title("Doom 1993", &doom, &e, None, None),
            Decision::Match("Q189784".into())
        );
        assert_eq!(
            pick_by_title("Doom Eternal", &doom, &e, None, None),
            Decision::None
        );
        // A single exact game label matches; the edition title resolves to the game.
        assert_eq!(
            pick_by_title("Papers, Please", &["Q14565978".into()], &e, None, None),
            Decision::Match("Q14565978".into())
        );
        assert_eq!(
            pick_by_title(
                "Fallout 3 Game of the Year Edition",
                &["Q108022844".into(), "Q221164".into()],
                &e,
                None,
                None
            ),
            Decision::Match("Q221164".into())
        );
        assert_eq!(platform_item("pc"), Some("Q1406"));
        assert_eq!(platform_item("unknown"), None);
    }
}
